use std::path::{Path, PathBuf};
use std::str::FromStr;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use domain::{Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource, Release};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::directory::transfer_plan;
use crate::management::{ApiError, ApiState};
use crate::scrape_store::ScrapeStoreExt;

#[derive(Deserialize)]
pub struct ClaimBody {
    path: String,
    title: String,
    kind: String,
    year: Option<u16>,
    tmdb_id: Option<String>,
}

pub async fn claim_unidentified(
    State(state): State<ApiState>,
    Json(body): Json<ClaimBody>,
) -> Result<Json<Value>, ApiError> {
    if body.title.trim().is_empty() {
        return Err(ApiError::invalid(
            "unidentified.invalid",
            "title is required".into(),
        ));
    }
    let kind = MediaKind::from_str(&body.kind).map_err(|_| {
        ApiError::invalid("unidentified.invalid", "kind must be movie or tv".into())
    })?;
    let src = PathBuf::from(&body.path);
    if !src.is_file() {
        return Err(ApiError::invalid(
            "unidentified.invalid",
            "Unidentified file is missing".into(),
        ));
    }
    let media = Media {
        id: MediaId::new(),
        kind,
        title: body.title.trim().to_string(),
        year: body.year,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: body.tmdb_id.filter(|id| !id.is_empty()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    let parsed = release::parse(
        src.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default(),
    );
    let release = Release {
        title: media.title.clone(),
        year: media.year.or(parsed.year),
        confidence: Confidence::High,
        ..parsed
    };
    let (root, mode, scrape) = {
        let store = state.store.lock();
        if !store
            .list_unidentified()?
            .iter()
            .any(|(path, _)| path == &body.path)
        {
            return Err(ApiError::missing(
                "unidentified.missing",
                "Unidentified not found".into(),
            ));
        }
        transfer_plan(&store, kind)?
    };
    let nfo = {
        let store = state.store.lock();
        let cfg = store
            .get_scrape_config()
            .map_err(|e| ApiError::invalid("unidentified.invalid", e.to_string()))?;
        scrape && cfg.effective.mirror_nfo
    };
    let naming = {
        let store = state.store.lock();
        store
            .naming_pattern(kind)
            .map_err(|e| ApiError::invalid("unidentified.invalid", e.to_string()))?
    };
    let dest = library::render_path(&root, &naming, &media, &release, &src)
        .map_err(|err| ApiError::invalid("unidentified.invalid", err.to_string()))?;
    // 没进台账不等于可以覆盖：未扫描、识别失败，或上次提交失败留下的视频
    // 都还在磁盘上。源和目标是同一文件（原地认领）才允许继续。
    if dest.exists() && dest != src {
        return Err(ApiError::with(
            StatusCode::CONFLICT,
            "unidentified.destination_exists",
            "媒体库中已存在相同目标文件，禁止覆盖已有文件".to_string(),
        ));
    }
    let requested = library::resolve_mode(&src, dest.parent().unwrap_or(Path::new(".")), mode);
    // Move 先复制、库记录成功后再删源。中途失败时源还在，同一请求可以重试。
    let staged = if requested == library::TransferMode::Move {
        library::TransferMode::Copy
    } else {
        requested
    };
    library::transfer_file(&src, &dest, staged)
        .map_err(|err| ApiError::invalid("unidentified.invalid", err.to_string()))?;
    let rollback_dest =
        (requested == library::TransferMode::Move && dest != src).then(|| dest.clone());
    let _ = library::scrape_beside(&dest, &media, nfo, None);
    if scrape {
        crate::poster_fetch::attach_poster(&state, &media, &dest);
    }
    let media = match state.store.lock().ensure_media(media) {
        Ok(media) => media,
        Err(error) => {
            if let Some(path) = &rollback_dest {
                let _ = std::fs::remove_file(path);
            }
            return Err(error.into());
        }
    };
    let row = LedgerRow {
        id: LedgerId::new(),
        media_id: media.id,
        path: dest.display().to_string(),
        season: release.season,
        episode: release.episode,
        resolution: release.resolution,
        codec: release.codec,
        hdr: release.hdr,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
    };
    {
        let store = state.store.lock();
        if let Err(error) = store.insert_ledger(&row) {
            if let Some(path) = &rollback_dest {
                let _ = std::fs::remove_file(path);
            }
            return Err(error.into());
        }
        if let Err(error) = store.delete_unidentified(&body.path) {
            tracing::warn!(path = %body.path, %error, "认领已入账，但未识别记录清理失败");
        }
    }
    if requested == library::TransferMode::Move && dest != src {
        if let Err(error) = std::fs::remove_file(&src) {
            tracing::warn!(src = %src.display(), %error, "认领已入账，但 Move 源文件清理失败");
        }
    }
    crate::http::library::enqueue_probes_for_rows(&state, std::slice::from_ref(&row));
    Ok(Json(json!({
        "path": row.path,
        "media_id": media.id.to_string(),
        "title": media.title,
    })))
}

pub async fn claim_unidentified_v1(
    state: State<ApiState>,
    body: Json<ClaimBody>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    match claim_unidentified(state, body).await {
        Ok(Json(data)) => crate::http::ok(data).into_response(),
        Err(error) => error.into_response(),
    }
}
