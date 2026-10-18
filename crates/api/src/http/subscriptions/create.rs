use std::str::FromStr;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use domain::{
    Coverage, FetchMode, FilterId, Media, MediaId, MediaKind, Subscribe, SubscribeId, UserId,
};

use super::types::*;
use super::views::subscription_json;
use crate::http::{err, ok};
use crate::management::ApiState;

pub(crate) async fn create_subscription(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<UserId>,
    Json(body): Json<CreateSubscriptionInput>,
) -> Response {
    let (media_input, kind_hint) = match resolve_media_input(&state, &body) {
        Ok(result) => result,
        Err(response) => return response,
    };
    let media_kind = match kind_hint {
        Some(kind) => kind,
        None => {
            return err(
                StatusCode::BAD_REQUEST,
                "subscription.invalid",
                "media.kind 必须是 movie 或 tv",
            );
        }
    };
    let fetch_mode = match FetchMode::from_str(&body.fetch_mode) {
        Ok(mode) => mode,
        Err(_) => {
            return err(
                StatusCode::BAD_REQUEST,
                "subscription.invalid",
                "fetch_mode 无效",
            );
        }
    };
    let coverage =
        match coverage_from_input(media_kind, body.coverage.clone(), body.full_season_pack) {
            Ok(coverage) => coverage,
            Err(response) => return response,
        };
    persist_subscription(
        state,
        user_id,
        media_kind,
        media_input,
        fetch_mode,
        coverage,
        body,
    )
}

fn resolve_media_input(
    state: &ApiState,
    body: &CreateSubscriptionInput,
) -> Result<(MediaInput, Option<MediaKind>), Response> {
    if let Some(raw) = &body.title_ref {
        return media_from_title_ref(state, raw);
    }
    let Some(media) = body.media.clone() else {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "subscription.invalid",
            "media 或 title_ref 必填",
        ));
    };
    let kind_hint = MediaKind::from_str(&media.kind).ok();
    Ok((media, kind_hint))
}

fn media_from_title_ref(
    state: &ApiState,
    raw: &str,
) -> Result<(MediaInput, Option<MediaKind>), Response> {
    if raw.trim().is_empty() {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "subscription.invalid",
            "title_ref 为空",
        ));
    }
    match crate::http::title_ref::resolve_title(state, raw) {
        Ok(Some(media)) => {
            let kind = media.kind;
            let identity = crate::http::title_ref::media_identity(&media);
            let identity: MediaInput = serde_json::from_value(identity).map_err(|_| {
                err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "store.error",
                    "media identity 序列化失败",
                )
            })?;
            Ok((identity, Some(kind)))
        }
        Ok(None) => Err(err(
            StatusCode::NOT_FOUND,
            "media.missing",
            "title_ref 无法解析到条目",
        )),
        Err(error) => Err(err(StatusCode::BAD_GATEWAY, "media.upstream", &error)),
    }
}

fn coverage_from_input(
    media_kind: MediaKind,
    coverage: Option<CoverageInput>,
    full_season_pack: bool,
) -> Result<Coverage, Response> {
    let coverage = match coverage {
        Some(CoverageInput::Movie) => Coverage::Movie,
        Some(CoverageInput::Tv {
            season,
            episode_from,
            episode_to,
        }) => Coverage::Tv {
            season,
            episode_from,
            episode_to,
        },
        None => {
            let default = crate::http::title_ref::default_coverage(media_kind);
            if matches!(media_kind, MediaKind::Movie) != matches!(default, Coverage::Movie) {
                return Err(err(
                    StatusCode::BAD_REQUEST,
                    "subscription.invalid",
                    "media.kind 与 coverage 不一致",
                ));
            }
            default
        }
    };
    if matches!(media_kind, MediaKind::Movie) != matches!(coverage, Coverage::Movie) {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "subscription.invalid",
            "media.kind 与 coverage 不一致",
        ));
    }
    coverage
        .validate(full_season_pack)
        .map_err(|message| err(StatusCode::BAD_REQUEST, "subscription.invalid", message))?;
    Ok(coverage)
}

fn persist_subscription(
    state: ApiState,
    user_id: UserId,
    media_kind: MediaKind,
    media_input: MediaInput,
    fetch_mode: FetchMode,
    coverage: Coverage,
    body: CreateSubscriptionInput,
) -> Response {
    let auto_next_season = body.auto_next_season;
    let pack_fallback_after_days = body.pack_fallback_after_days;
    let incoming = incoming_media(media_kind, media_input);
    let subscribe =
        match subscribe_from_input(&state, user_id, incoming.id, fetch_mode, coverage, &body) {
            Ok(subscribe) => subscribe,
            Err(response) => return response,
        };
    let (media, subscribe) =
        match crate::subscribe_create::create(&state, incoming, subscribe, None) {
            Ok(result) => result,
            Err(crate::subscribe_create::CreateError::Overlap(other)) => {
                return err(
                    StatusCode::CONFLICT,
                    "subscription.overlap",
                    &format!("同一媒体库中已有覆盖重叠的订阅（{other}），请合并或调整集数范围"),
                );
            }
            Err(error) => {
                return err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "subscription.create_failed",
                    &error.to_string(),
                );
            }
        };
    if let Some(days) = pack_fallback_after_days {
        crate::pack_fallback::set_after_days(&state, subscribe.id, Some(days));
    }
    if auto_next_season {
        crate::completion::set_auto_next_season(&state, subscribe.id, true);
    }
    let facts = {
        let store = state.store.lock();
        store.load_subscribe_facts(subscribe.id).unwrap_or_default()
    };
    let body = {
        let store = state.store.lock();
        subscription_json(&store, state.catalog.as_ref(), &subscribe, &media, &facts)
    };
    (StatusCode::CREATED, ok(body)).into_response()
}

fn subscribe_from_input(
    state: &ApiState,
    user_id: UserId,
    media_id: MediaId,
    fetch_mode: FetchMode,
    coverage: Coverage,
    body: &CreateSubscriptionInput,
) -> Result<Subscribe, Response> {
    let store = state.store.lock();
    let filter_id = resolve_filter_id(&store, body)?;
    let filter_keep_old = store
        .get_filter(filter_id)
        .ok()
        .flatten()
        .map(|f| f.keep_old_versions)
        .unwrap_or(false);
    let effective_keep_old = body.keep_old_versions.unwrap_or(filter_keep_old);
    Ok(Subscribe {
        id: SubscribeId::new(),
        user_id,
        media_id,
        coverage,
        fetch_mode,
        filter_id,
        wash_cut: body.wash_cut,
        keep_old_versions: effective_keep_old,
        wash_cut_filter_id: optional_filter_id(&store, body.wash_cut_filter_id.as_deref())?,
        full_season_pack: body.full_season_pack,
        downloader_id: optional_downloader_id(&store, body.downloader_id.as_deref())?,
        library_id: body
            .library_id
            .as_deref()
            .filter(|s| !s.is_empty())
            .and_then(|s| domain::LibraryId::from_str(s).ok()),
        tracking_state: "active".into(),
        follow_future: body.follow_future,
        search_interval_secs: match body.search_interval_secs {
            Some(secs) => crate::http::subscribe_schedule::clamp_interval(secs)
                .map_err(|message| err(StatusCode::BAD_REQUEST, "subscription.invalid", message))?,
            None => crate::http::subscribe_schedule::default_interval(),
        },
    })
}

fn incoming_media(media_kind: MediaKind, media_input: MediaInput) -> Media {
    Media {
        id: MediaId::new(),
        kind: media_kind,
        title: media_input.title,
        year: media_input.year,
        original_title: media_input.original_title.filter(|t| !t.is_empty()),
        aliases: Vec::new(),
        tmdb_id: media_input.tmdb_id.filter(|id| !id.is_empty()),
        douban_id: media_input.douban_id.filter(|id| !id.is_empty()),
        tvdb_id: media_input.tvdb_id.filter(|id| !id.is_empty()),
        bangumi_id: media_input.bangumi_id.filter(|id| !id.is_empty()),
        anilist_id: media_input.anilist_id.filter(|id| !id.is_empty()),
    }
}

fn resolve_filter_id(
    store: &crate::Store,
    body: &CreateSubscriptionInput,
) -> Result<FilterId, Response> {
    match &body.filter_id {
        Some(raw) => existing_filter_id(store, raw),
        None => inherited_filter_id(store, body.library_id.as_deref()),
    }
}

fn inherited_filter_id(
    store: &crate::Store,
    library_id: Option<&str>,
) -> Result<FilterId, Response> {
    let id = library_id
        .filter(|id| !id.is_empty())
        .and_then(|id| store.get_library(id).ok().flatten())
        .and_then(|lib| lib.default_filter_id)
        .and_then(|id_str| FilterId::from_str(&id_str).ok());
    let id = if let Some(id) = id {
        id
    } else {
        match crate::management::default_filter_id(store) {
            Ok(Some(id)) => id,
            Ok(None) => {
                return Err(err(
                    StatusCode::BAD_REQUEST,
                    "subscription.invalid",
                    "未配置默认 Filter，请提供 filter_id",
                ));
            }
            Err(error) => return Err(error.into_response()),
        }
    };
    existing_filter(store, id, "默认 Filter 不存在，请提供 filter_id")
}

fn existing_filter_id(store: &crate::Store, raw: &str) -> Result<FilterId, Response> {
    let id = FilterId::from_str(raw).map_err(|_| {
        err(
            StatusCode::BAD_REQUEST,
            "subscription.invalid",
            "filter_id 无效",
        )
    })?;
    existing_filter(store, id, "filter_id 无效")
}

fn existing_filter(
    store: &crate::Store,
    id: FilterId,
    missing: &str,
) -> Result<FilterId, Response> {
    match store.get_filter(id) {
        Ok(Some(_)) => Ok(id),
        Ok(None) => Err(err(
            StatusCode::BAD_REQUEST,
            "subscription.invalid",
            missing,
        )),
        Err(error) => Err(err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        )),
    }
}

fn optional_filter_id(
    store: &crate::Store,
    raw: Option<&str>,
) -> Result<Option<FilterId>, Response> {
    let Some(raw) = raw.filter(|id| !id.is_empty()) else {
        return Ok(None);
    };
    let id = FilterId::from_str(raw).map_err(|_| {
        err(
            StatusCode::BAD_REQUEST,
            "subscription.invalid",
            "wash_cut_filter_id 无效",
        )
    })?;
    existing_filter(store, id, "wash_cut_filter_id 无效").map(Some)
}

fn optional_downloader_id(
    store: &crate::Store,
    raw: Option<&str>,
) -> Result<Option<domain::DownloaderId>, Response> {
    let Some(raw) = raw.filter(|id| !id.is_empty()) else {
        return Ok(None);
    };
    let id = domain::DownloaderId::from_str(raw).map_err(|_| {
        err(
            StatusCode::BAD_REQUEST,
            "subscription.invalid",
            "downloader_id 无效",
        )
    })?;
    match store.get_downloader(id) {
        Ok(Some(_)) => Ok(Some(id)),
        Ok(None) => Err(err(
            StatusCode::BAD_REQUEST,
            "subscription.invalid",
            "downloader_id 无效",
        )),
        Err(error) => Err(err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        )),
    }
}
