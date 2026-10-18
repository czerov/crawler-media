use axum::Json;
use axum::extract::State;
use domain::SubscribeId;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::management::{ApiError, ApiState};

#[derive(Deserialize)]
pub struct AdmitBody {
    subscribe_id: String,
    enclosure: String,
    #[serde(default)]
    release: Option<ReleaseOverride>,
}

#[derive(Deserialize)]
pub struct ReleaseOverride {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    year: Option<u16>,
    #[serde(default)]
    season: Option<u32>,
    #[serde(default)]
    episode: Option<u32>,
    #[serde(default)]
    resolution: Option<String>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    codec: Option<String>,
    #[serde(default)]
    hdr: Option<String>,
}

fn apply_admit_release_override(
    base: domain::Release,
    override_rel: &ReleaseOverride,
) -> domain::Release {
    domain::Release {
        title: override_rel.title.clone().unwrap_or(base.title),
        year: override_rel.year.or(base.year),
        season: override_rel.season.or(base.season),
        episode: override_rel.episode.or(base.episode),
        episode_to: base.episode_to,
        resolution: override_rel.resolution.clone().or(base.resolution),
        source: override_rel.source.clone().or(base.source),
        codec: override_rel.codec.clone().or(base.codec),
        hdr: override_rel.hdr.clone().or(base.hdr),
        subtitle_language: base.subtitle_language,
        audio_language: base.audio_language,
        group: base.group,
        confidence: domain::Confidence::High,
        season_inferred: false,
    }
}

fn load_admit_context(
    store: &store::Store,
    id: SubscribeId,
) -> Result<
    (
        domain::Subscribe,
        domain::Media,
        domain::Filter,
        Vec<domain::Site>,
    ),
    ApiError,
> {
    let subscribe = store
        .get_subscribe(id)?
        .ok_or_else(|| ApiError::missing("subscribe.missing", "Subscribe not found".into()))?;
    let media = store
        .get_media(subscribe.media_id)?
        .ok_or_else(|| ApiError::missing("media.missing", "Media not found".into()))?;
    let filter = store
        .get_filter(subscribe.filter_id)?
        .ok_or_else(|| ApiError::missing("filter.missing", "Filter not found".into()))?;
    let sites = store.list_enabled_sites()?;
    Ok((subscribe, media, filter, sites))
}

pub async fn admit_torrent(
    State(state): State<ApiState>,
    Json(body): Json<AdmitBody>,
) -> Result<Json<Value>, ApiError> {
    let subscribe_id = body
        .subscribe_id
        .parse::<SubscribeId>()
        .map_err(|_| ApiError::invalid("subscribe.invalid", "invalid Subscribe id".into()))?;
    let (subscribe, media, filter, sites) = load_admit_context(&state.store.lock(), subscribe_id)?;
    let keyword = media
        .original_title
        .as_deref()
        .filter(|t| !t.is_empty())
        .unwrap_or(&media.title);
    let torrents = state.indexer.search(&sites, keyword).torrents;
    let Some(chosen) = torrents
        .into_iter()
        .find(|torrent| torrent.enclosure == body.enclosure)
    else {
        return Err(ApiError::invalid(
            "torrent.missing",
            "Torrent not found".into(),
        ));
    };
    let mut corrected = release::parse(&chosen.title);
    if let Some(override_release) = &body.release {
        corrected = apply_admit_release_override(corrected, override_release);
    }
    let override_for_pending = body.release.as_ref().map(|_| corrected.clone());
    let admitted = filter::admit_scored(vec![(chosen.clone(), Some(corrected))], &filter).admitted;
    let Some(scored) = admitted.into_iter().next() else {
        return Err(ApiError::invalid(
            "subscribe.rejected",
            "Torrent rejected by Filter".into(),
        ));
    };
    let downloader_id = crate::delivery::target_id(&state, Some(&subscribe), &scored.torrent)
        .map_err(|err| ApiError::invalid("subscribe.invalid", err.to_string()))?;
    crate::delivery::submit_torrent(&state, &scored.torrent, downloader_id)
        .map_err(|err| ApiError::invalid("subscribe.invalid", err.to_string()))?;
    state.store.lock().record_manual_pending_submissions(
        subscribe.id,
        &[(
            scored.score,
            crate::store::PendingDownload {
                submitted_at: None,
                torrent: scored.torrent.clone(),
                release_override: override_for_pending,
                downloader_id,
            },
        )],
    )?;
    Ok(Json(json!({
        "subscribe_id": subscribe.id.to_string(),
        "title": scored.torrent.title,
        "enclosure": scored.torrent.enclosure,
        "score": scored.score,
    })))
}

pub async fn admit_torrent_v1(
    state: State<ApiState>,
    body: Json<AdmitBody>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    match admit_torrent(state, body).await {
        Ok(Json(data)) => crate::http::ok(data).into_response(),
        Err(error) => error.into_response(),
    }
}
