//! Scrape & organize settings: read/write the `metadata.scrape` config domain
//! and a naming-template live preview. Empty fields follow defaults; the
//! response carries both `setting` (stored) and `effective` (merged).

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use domain::{Media, MediaId, MediaKind, Release};
use serde_json::{Value, json};

use crate::catalog::Catalog;
use crate::http::{err, ok};
use crate::management::ApiState;
use crate::scrape_config::{ScrapeConfigSetting, compose_config};
use crate::scrape_store::ScrapeStoreExt;

fn bad(code: &str, message: &str) -> Response {
    err(StatusCode::BAD_REQUEST, code, message)
}

fn validate(setting: &ScrapeConfigSetting) -> Result<(), String> {
    if setting
        .language_priority
        .iter()
        .any(|s| s.trim().is_empty())
    {
        return Err("元数据语言优先级不能包含空项".into());
    }
    if setting
        .cert_country_priority
        .iter()
        .any(|s| s.trim().is_empty())
    {
        return Err("分级优先地区不能包含空项".into());
    }
    if !matches!(setting.poster_mode.as_str(), "" | "default" | "language") {
        return Err("海报模式必须是 default 或 language".into());
    }
    if let Some(secs) = setting.fingerprint_duration_secs {
        if secs < 30 || secs > 3600 {
            return Err("声纹采样时长需要在 30~3600 秒之间".into());
        }
    }
    if let Some(mode) = &setting.fingerprint_sampling_mode {
        let trimmed = mode.trim();
        if !trimmed.is_empty() && trimmed != "full_window" && trimmed != "adaptive" {
            return Err("声纹采样模式必须是 full_window 或 adaptive".into());
        }
    }
    for (key, min) in [
        ("poster_min_width", setting.poster_min_width),
        ("backdrop_min_width", setting.backdrop_min_width),
    ] {
        if min.is_some_and(|v| v == 0 || v > 20_000) {
            return Err(format!("{key} 需要在 1~20000 之间（0 表示关闭门槛）"));
        }
    }
    // 每个命名模板只描述一段目录/文件名：禁止路径分隔符。
    let segments = [
        ("naming_entry_dir", &setting.naming_entry_dir),
        ("naming_movie_file", &setting.naming_movie_file),
        ("naming_season_dir", &setting.naming_season_dir),
        ("naming_episode_file", &setting.naming_episode_file),
    ];
    for (key, value) in segments {
        if value.contains('/') {
            return Err(format!("{key} 不能包含路径分隔符 /"));
        }
    }
    let composed = compose_config(setting.clone());
    let effective = &composed.effective;
    let movie = effective.compose_movie_pattern();
    let tv = effective.compose_tv_pattern();
    library::validate_pattern(&movie).map_err(|e| e.to_string())?;
    library::validate_pattern(&tv).map_err(|e| e.to_string())?;
    let entry = if effective.naming_entry_dir.is_empty() {
        crate::scrape_config::default_naming_entry_dir()
    } else {
        &effective.naming_entry_dir
    };
    if !entry.contains("{title}") && !entry.contains("{original_title}") {
        return Err("条目目录模板必须包含 {title} 或 {original_title}".into());
    }
    let season = if effective.naming_season_dir.is_empty() {
        crate::scrape_config::default_naming_season_dir()
    } else {
        &effective.naming_season_dir
    };
    if !season.contains("{season}") {
        return Err("季目录模板必须包含 {season}".into());
    }
    let episode = if effective.naming_episode_file.is_empty() {
        crate::scrape_config::default_naming_episode_file()
    } else {
        &effective.naming_episode_file
    };
    let has_se = episode.contains("{season_episode}");
    let has_both = episode.contains("{season}") && episode.contains("{episode}");
    if !has_se && !has_both {
        return Err(
            "剧集文件名模板必须包含 {season_episode}，或同时包含 {season} 与 {episode}".into(),
        );
    }
    Ok(())
}

/// GET /settings/scrape — `{setting, effective}`.
pub(crate) async fn get_scrape_settings(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
) -> Response {
    let store = state.store.lock();
    let is_admin = store
        .user_role(user_id)
        .map(|role| role == "admin")
        .unwrap_or(false);
    match store.get_scrape_config() {
        Ok(mut config) => {
            if !is_admin {
                config.setting.theintrodb_api_key = None;
                config.setting.fanart_api_key = None;
            }
            ok(scrape_json(&config)).into_response()
        }
        Err(error) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        ),
    }
}

/// PUT /settings/scrape — save + validate; unknown fields are dropped.
pub(crate) async fn put_scrape_settings(
    State(state): State<ApiState>,
    Json(body): Json<Value>,
) -> Response {
    let setting = match serde_json::from_value::<ScrapeConfigSetting>(body["setting"].clone()) {
        Ok(setting) => setting,
        Err(_) => return bad("scrape.invalid", "设置格式无效"),
    };
    if let Err(message) = validate(&setting) {
        return bad("scrape.invalid", &message);
    }
    let store = state.store.lock();
    match store.save_scrape_config(&setting) {
        Ok(config) => ok(scrape_json(&config)).into_response(),
        Err(error) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        ),
    }
}

/// POST /settings/scrape/preview-naming — render both composed patterns with a
/// sample movie + sample episode so the UI can preview before saving.
pub(crate) async fn preview_naming(State(state): State<ApiState>) -> Response {
    let store = state.store.lock();
    let config = match store.get_scrape_config() {
        Ok(config) => config,
        Err(error) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            );
        }
    };
    let movie_media = sample_media(MediaKind::Movie);
    let tv_media = sample_media(MediaKind::Tv);
    let movie_release = sample_release(MediaKind::Movie);
    let tv_release = sample_release(MediaKind::Tv);
    let render = |pattern: &str, media: &Media, release: &Release| {
        library::render_path(
            std::path::Path::new(""),
            pattern,
            media,
            release,
            std::path::Path::new("dummy.mkv"),
        )
        .map(|p| p.display().to_string())
        .unwrap_or_default()
    };
    ok(json!({
        "movie": render(&config.effective.compose_movie_pattern(), &movie_media, &movie_release),
        "tv": render(&config.effective.compose_tv_pattern(), &tv_media, &tv_release),
        "movie_media_title": movie_media.title,
        "tv_media_title": tv_media.title,
    }))
    .into_response()
}

fn scrape_json(config: &crate::scrape_config::ScrapeConfig) -> Value {
    let e = &config.effective;
    json!({
        "setting": config.setting,
        "effective": {
            "language_priority": e.language_priority,
            "cert_country_priority": e.cert_country_priority,
            "poster_mode": e.poster_mode,
            "poster_language_priority": e.poster_language_priority,
            "backdrop_language_priority": e.backdrop_language_priority,
            "poster_min_width": e.poster_min_width,
            "backdrop_min_width": e.backdrop_min_width,
            "poster_size": e.poster_size,
            "backdrop_size": e.backdrop_size,
            "still_size": e.still_size,
            "naming_entry_dir": e.naming_entry_dir,
            "naming_movie_file": e.naming_movie_file,
            "naming_season_dir": e.naming_season_dir,
            "naming_episode_file": e.naming_episode_file,
            "mirror_images": e.mirror_images,
            "mirror_nfo": e.mirror_nfo,
            "mirror_episode_thumbs": e.mirror_episode_thumbs,
            "theintrodb_enabled": e.theintrodb_enabled,
            "fanart_configured": e.fanart_api_key.is_some(),
            "fingerprint_duration_secs": e.fingerprint_duration_secs,
        },
    })
}

fn sample_media(kind: MediaKind) -> Media {
    Media {
        id: MediaId::new(),
        kind,
        title: "黑客帝国".into(),
        year: Some(1999),
        original_title: Some("The Matrix".into()),
        aliases: Vec::new(),
        tmdb_id: Some("603".into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    }
}

fn sample_release(kind: MediaKind) -> Release {
    Release {
        title: "The.Matrix.1999.2160p".into(),
        year: Some(1999),
        season: (kind == MediaKind::Tv).then_some(1),
        episode: (kind == MediaKind::Tv).then_some(3),
        episode_to: None,
        resolution: Some("2160p".into()),
        source: Some("WEB-DL".into()),
        codec: Some("HEVC".into()),
        hdr: Some("HDR10".into()),
        subtitle_language: None,
        audio_language: None,
        group: Some("OurTV".into()),
        confidence: domain::Confidence::High,
        season_inferred: false,
    }
}

/// GET /settings/languages — TMDB configuration 语种全表（设置页「更多语言」）。
pub(crate) async fn languages(State(state): State<ApiState>) -> Response {
    let catalog: &dyn Catalog = state.catalog.as_ref();
    let rows = catalog.configuration_languages().unwrap_or_default();
    let languages: Vec<Value> = rows
        .into_iter()
        .map(|row| {
            json!({
                "code": row.code,
                "name": row.name,
                "english_name": row.english_name,
            })
        })
        .collect();
    ok(json!({ "languages": languages })).into_response()
}

/// GET /settings/countries — TMDB configuration 地区全表（设置页「更多地区」）。
pub(crate) async fn countries(State(state): State<ApiState>) -> Response {
    let catalog: &dyn Catalog = state.catalog.as_ref();
    let rows = catalog.configuration_countries().unwrap_or_default();
    let countries: Vec<Value> = rows
        .into_iter()
        .map(|row| {
            json!({
                "code": row.code,
                "name": row.native_name,
                "english_name": row.english_name,
            })
        })
        .collect();
    ok(json!({ "countries": countries })).into_response()
}
