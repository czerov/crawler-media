use std::str::FromStr;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use domain::{DownloaderId, Release, Torrent, UserId};
use serde_json::{Value, json};

use super::tasks::SubmitInput;
use crate::http::{err, ok};
use crate::management::ApiState;

fn resolve_subscribe_and_check_owner(
    state: &ApiState,
    subscribe_id_raw: Option<&str>,
    user_id: Option<UserId>,
    torrent: &Torrent,
    parsed: &Release,
) -> Result<Option<domain::Subscribe>, Response> {
    let Some(raw) = subscribe_id_raw else {
        return Ok(None);
    };
    let id = domain::SubscribeId::from_str(raw)
        .map_err(|_| err(StatusCode::BAD_REQUEST, "subscribe.invalid", "订阅 id 无效"))?;
    let loaded = state.store.lock().get_subscribe(id);
    let subscribe = match loaded {
        Ok(Some(s)) => {
            if let Some(uid) = user_id {
                let store = state.store.lock();
                let is_admin = store.user_role(uid).map(|r| r == "admin").unwrap_or(false);
                if !is_admin && s.user_id != uid {
                    return Err(err(
                        StatusCode::FORBIDDEN,
                        "auth.forbidden",
                        "无权操作他人的订阅",
                    ));
                }
            }
            s
        }
        Ok(None) => {
            return Err(err(
                StatusCode::NOT_FOUND,
                "subscribe.missing",
                "订阅不存在",
            ));
        }
        Err(e) => {
            return Err(err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &e.to_string(),
            ));
        }
    };
    let media = match state.store.lock().get_media(subscribe.media_id) {
        Ok(Some(m)) => m,
        Ok(None) => return Err(err(StatusCode::NOT_FOUND, "media.missing", "媒体不存在")),
        Err(e) => {
            return Err(err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &e.to_string(),
            ));
        }
    };
    if !subscribe::candidate_matches_subscribe(&subscribe, &media, parsed) {
        tracing::error!(subscribe_id = %subscribe.id, media = %media.title, torrent = %torrent.title, "手动投递的种子与订阅不匹配");
        return Err(err(
            StatusCode::BAD_REQUEST,
            "subscribe.torrent_mismatch",
            "种子与订阅媒体或范围不匹配",
        ));
    }
    Ok(Some(subscribe))
}

fn resolve_downloader_and_save_path(
    state: &ApiState,
    subscribe: Option<&domain::Subscribe>,
    torrent: &Torrent,
    explicit_id: Option<DownloaderId>,
    body_save_path: Option<String>,
    auto_route: bool,
) -> Result<(Option<DownloaderId>, Option<String>, Option<String>), Response> {
    let downloader_id = match explicit_id {
        Some(id)
            if state
                .store
                .lock()
                .get_downloader(id)
                .ok()
                .flatten()
                .is_some() =>
        {
            Some(id)
        }
        Some(_) => {
            return Err(err(
                StatusCode::NOT_FOUND,
                "downloader.missing",
                "下载器不存在",
            ));
        }
        None => match crate::delivery::target_id(state, subscribe, torrent) {
            Ok(id) => id,
            Err(e) => {
                return Err(err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "downloader.error",
                    &e.to_string(),
                ));
            }
        },
    };
    let save_path = if auto_route {
        let Some(host_path) = body_save_path.as_deref().filter(|p| !p.trim().is_empty()) else {
            return Err(err(
                StatusCode::BAD_REQUEST,
                "download.route",
                "自动入库必须提供预演得到的保存目录",
            ));
        };
        let Some(id) = downloader_id else {
            return Err(err(
                StatusCode::BAD_REQUEST,
                "downloader.missing",
                "没有可用的下载器",
            ));
        };
        let maps = match state.store.lock().get_downloader(id) {
            Ok(Some(row)) => row.path_maps,
            Ok(None) => {
                return Err(err(
                    StatusCode::NOT_FOUND,
                    "downloader.missing",
                    "下载器不存在",
                ));
            }
            Err(e) => {
                return Err(err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "store.error",
                    &e.to_string(),
                ));
            }
        };
        let path = std::path::Path::new(host_path);
        Some(
            maps.iter()
                .find_map(|m| m.remap_to_downloader(path))
                .unwrap_or_else(|| host_path.to_string()),
        )
    } else {
        body_save_path.clone().filter(|p| !p.trim().is_empty())
    };
    let response_save_path = body_save_path;
    Ok((downloader_id, save_path, response_save_path))
}

fn record_submitted_pending(
    state: &ApiState,
    subscribe: &domain::Subscribe,
    torrent: &Torrent,
    downloader_id: Option<DownloaderId>,
    parsed: &Release,
    score: i32,
) -> Result<(), Response> {
    let store = state.store.lock();
    if let Err(error) = store.record_pending_submission(
        subscribe.id,
        score,
        &crate::store::PendingDownload {
            torrent: torrent.clone(),
            release_override: None,
            downloader_id,
            submitted_at: Some(crate::job_loop::unix_now()),
        },
    ) {
        tracing::error!(%error, subscribe_id = %subscribe.id, torrent = %torrent.title, "保存已提交的种子失败");
        return Err(err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        ));
    }
    let units = parsed.covered_episodes();
    if units.is_empty() {
        let _ = store.record_wanted_grab(
            subscribe.id,
            parsed.season,
            parsed.episode,
            &torrent.title,
            crate::job_loop::unix_now(),
        );
    } else {
        for (season, episode) in units {
            let _ = store.record_wanted_grab(
                subscribe.id,
                Some(season),
                Some(episode),
                &torrent.title,
                crate::job_loop::unix_now(),
            );
        }
    }
    Ok(())
}

enum SubmitFailure {
    Conflict,
    Downloader(downloader::DownloaderError),
    Response(Response),
}

/// D-M5: any slot of `parsed` already owned or in flight for `subscribe`.
fn slot_taken(state: &ApiState, subscribe: &domain::Subscribe, parsed: &Release) -> bool {
    let store = state.store.lock();
    let Ok(Some(media)) = store.get_media(subscribe.media_id) else {
        return false;
    };
    let (Ok(facts), Ok(pending)) = (
        store.load_library_subscribe_facts(subscribe, media.kind),
        store.load_pending(subscribe.id),
    ) else {
        return false;
    };
    drop(store);
    let releases = crate::worker::finish_search::pending_releases(&pending);
    let facts = subscribe::facts_with_pending(facts, &releases, subscribe, &media);
    subscribe::pending_slots(subscribe, parsed)
        .iter()
        .any(|(s, e)| facts.get(*s, *e).is_some())
}

/// D-M5: score the manual torrent with the subscription's filter (0 if rejected).
fn filter_score(state: &ApiState, subscribe: &domain::Subscribe, torrent: &Torrent) -> i32 {
    let Ok(Some(filter)) = state.store.lock().get_filter(subscribe.filter_id) else {
        return 0;
    };
    filter::admit(vec![torrent.clone()], &filter)
        .admitted
        .first()
        .map_or(0, |scored| scored.score)
}

fn build_submit_torrent(body: &SubmitInput) -> (Torrent, Release, Vec<Value>) {
    let torrent = Torrent {
        site_id: body
            .site_id
            .as_deref()
            .and_then(|id| domain::SiteId::from_str(id).ok())
            .unwrap_or_else(domain::SiteId::new),
        title: body.title.clone(),
        enclosure: body.download_url.clone(),
        size_bytes: body.size_bytes,
        seeders: None,
        free: false,
        hr: false,
        imdb_id: None,
        id: body.torrent_id.clone(),
        leechers: None,
        snatched: None,
        upload_time: body.publish_time.clone(),
        detail_url: None,
        category: body.category.clone(),
        poster_url: None,
    };
    let parsed = release::parse(&torrent.title);
    let units = match parsed.covered_episodes().as_slice() {
        [] if parsed.season.is_some() => {
            vec![json!({ "season_number": parsed.season, "episode_number": 0 })]
        }
        [] => vec![json!({ "season_number": 0, "episode_number": 0 })],
        episodes => episodes
            .iter()
            .map(|(season, episode)| json!({ "season_number": season, "episode_number": episode }))
            .collect(),
    };
    (torrent, parsed, units)
}

pub(crate) async fn submit(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<Option<UserId>>,
    Json(body): Json<SubmitInput>,
) -> Response {
    // H-1: manual submission writes pending rows.
    if let Err(response) = state.require_writable() {
        return response;
    }
    if body
        .site_id
        .as_deref()
        .is_some_and(|id| domain::SiteId::from_str(id).is_err())
    {
        return err(StatusCode::BAD_REQUEST, "site.invalid", "站点 id 无效");
    }
    let (torrent, parsed, units) = build_submit_torrent(&body);
    let explicit_id = match body
        .downloader_id
        .as_deref()
        .map(DownloaderId::from_str)
        .transpose()
    {
        Ok(id) => id,
        Err(_) => {
            return err(
                StatusCode::BAD_REQUEST,
                "downloader.invalid",
                "下载器 id 无效",
            );
        }
    };
    let subscribe = match resolve_subscribe_and_check_owner(
        &state,
        body.subscribe_id.as_deref(),
        user_id,
        &torrent,
        &parsed,
    ) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let (downloader_id, save_path, response_save_path) = match resolve_downloader_and_save_path(
        &state,
        subscribe.as_ref(),
        &torrent,
        explicit_id,
        body.save_path,
        body.auto_route,
    ) {
        Ok(res) => res,
        Err(r) => return r,
    };
    let delivery_state = state.clone();
    let delivery_torrent = torrent.clone();
    let delivery_subscribe = subscribe.clone();
    let delivery_parsed = parsed.clone();
    let force = body.force;
    let add_result = tokio::task::spawn_blocking(move || -> Result<(), SubmitFailure> {
        let state = &delivery_state;
        let torrent = &delivery_torrent;
        // D-M5: serialize with the subscription's own search/Transfer.
        let guard = delivery_subscribe
            .as_ref()
            .map(|sub| state.subscribe_guard(sub.id));
        let _held = guard.as_ref().map(|g| g.lock());
        let score = match &delivery_subscribe {
            Some(sub) => {
                if !force && slot_taken(state, sub, &delivery_parsed) {
                    return Err(SubmitFailure::Conflict);
                }
                filter_score(state, sub, torrent)
            }
            None => 0,
        };
        crate::delivery::submit_torrent_with_options(
            state,
            torrent,
            downloader_id,
            save_path.as_deref(),
        )
        .map_err(SubmitFailure::Downloader)?;
        if let Some(sub) = &delivery_subscribe {
            if let Err(response) = record_submitted_pending(
                state,
                sub,
                torrent,
                downloader_id,
                &delivery_parsed,
                score,
            ) {
                // D-M5: undo the submission so no untracked task lingers.
                if let Ok(client) = crate::delivery::client_for_id(state, downloader_id) {
                    let _ = client.remove_owned(torrent, false);
                }
                return Err(SubmitFailure::Response(response));
            }
        }
        Ok(())
    })
    .await
    .unwrap_or_else(|e| {
        Err(SubmitFailure::Downloader(
            downloader::DownloaderError::Message(e.to_string()),
        ))
    });

    let add_result = match add_result {
        Err(SubmitFailure::Conflict) => {
            return err(
                StatusCode::CONFLICT,
                "subscribe.slot_taken",
                "该集已在库或正在下载；确认后可用 force=true 强制投递",
            );
        }
        Err(SubmitFailure::Response(response)) => return response,
        Err(SubmitFailure::Downloader(e)) => Err(e),
        Ok(()) => Ok(()),
    };
    match add_result {
        Ok(_) => {
            if let Some(cat) = body.category.as_deref().filter(|c| !c.trim().is_empty()) {
                let pref = json!({
                    "kind": if body.auto_route { "smart" } else if response_save_path.is_some() { "dir" } else { "default" },
                    "save_path": response_save_path,
                    "downloader_id": downloader_id.map(|id| id.to_string()),
                    "downloader_name": Value::Null,
                    "updated_at": crate::store::now_rfc3339(),
                });
                let store = state.store.lock();
                let raw = store.get_setting("download.target_prefs").ok().flatten();
                let mut prefs = raw
                    .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                    .unwrap_or_else(|| json!({}));
                prefs[cat] = pref;
                let _ = store.put_setting("download.target_prefs", &prefs.to_string());
            }
            tracing::info!(torrent = %torrent.title, downloader_id = ?downloader_id, "种子已提交给下载器");
            ok(json!({ "ok": true, "save_path": response_save_path, "units": units }))
                .into_response()
        }
        Err(e) => {
            tracing::error!(%e, torrent = %torrent.title, downloader_id = ?downloader_id, "提交种子失败");
            err(
                StatusCode::BAD_REQUEST,
                "downloader.add_failed",
                &e.to_string(),
            )
        }
    }
}
