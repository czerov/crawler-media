//! Library entity CRUD: create / rename / re-root / delete / default / reorder.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use domain::MediaKind;
use serde_json::{Value, json};
use std::str::FromStr;

use crate::http::{err, ok};
use crate::management::ApiState;
use crate::store::StoreError;

use super::library::{library_json, missing_library};

fn parse_member_ids(body: &Value) -> Vec<domain::UserId> {
    body["member_ids"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().and_then(|s| domain::UserId::from_str(s).ok()))
                .collect()
        })
        .unwrap_or_default()
}

fn paths_from(body: &Value) -> Vec<String> {
    body["root_paths"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.trim().to_string()))
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// POST /libraries — create a library with one or more ordered roots
/// (first = primary). Payload fields beyond name/kind/root_paths are ignored.
pub(crate) async fn create_library(
    State(state): State<ApiState>,
    Json(body): Json<Value>,
) -> Response {
    let kind = match MediaKind::from_str(body["kind"].as_str().unwrap_or_default()) {
        Ok(kind) => kind,
        Err(_) => {
            return err(
                StatusCode::BAD_REQUEST,
                "library.invalid_kind",
                "kind 必须是 movie / tv / video",
            );
        }
    };
    let name = body["name"].as_str().unwrap_or_default().trim().to_string();
    if name.is_empty() {
        return err(
            StatusCode::BAD_REQUEST,
            "library.invalid_name",
            "库名称不能为空",
        );
    }
    let root_paths = paths_from(&body);
    if root_paths.is_empty() {
        return err(
            StatusCode::BAD_REQUEST,
            "library.invalid_roots",
            "至少需要一个目录",
        );
    }
    let refs: Vec<&str> = root_paths.iter().map(String::as_str).collect();
    let access_mode = body["access_mode"].as_str().unwrap_or("everyone");
    let admin_visible = body["admin_visible"].as_bool().unwrap_or(true);
    let member_ids = parse_member_ids(&body);
    let match_rules = body["match_rules"].as_array().cloned().unwrap_or_default();
    let default_filter_id = body
        .get("default_filter_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let store = state.store.lock();
    match store.create_library_full(
        kind,
        &name,
        &refs,
        access_mode,
        admin_visible,
        &member_ids,
        &match_rules,
        default_filter_id,
    ) {
        Ok(library) => {
            let lib_id = library.id.clone();
            let _json_resp = library_json(&store, &library);
            if let Err(error) =
                store.set_library_intro_settings(&library.id, false, false)
            {
                tracing::warn!(%error, "保存新建媒体库片头片尾开关失败");
            }
            if let Err(error) = store.set_library_switch_settings(
                &library.id,
                body.get("realtime_watch").and_then(|v| v.as_bool()),
                body.get("generate_thumbnails").and_then(|v| v.as_bool()),
                body.get("extract_chapter_images").and_then(|v| v.as_bool()),
                body.get("exclude_from_home").and_then(|v| v.as_bool()),
                body.get("auto_series_collections")
                    .and_then(|v| v.as_bool()),
            ) {
                tracing::warn!(%error, "保存新建媒体库附加开关失败");
            }
            let updated = store
                .get_library(&library.id)
                .ok()
                .flatten()
                .unwrap_or(library);
            let json_resp = library_json(&store, &updated);
            drop(store);

            // 自动调度首次扫描，建库后不用手动去点扫描
            let scan_state = state.clone();
            tokio::spawn(async move {
                let _ = crate::http::library_scan::scan_library(
                    axum::extract::State(scan_state),
                    axum::extract::Path(lib_id),
                )
                .await;
            });

            ok(json_resp).into_response()
        }
        Err(error) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        ),
    }
}

/// PATCH /libraries/{id} — rename and/or replace the root list.
pub(crate) async fn patch_library(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let name = body["name"].as_str().map(str::to_string);
    let root_paths = body["root_paths"].is_array().then(|| paths_from(&body));
    if let Some(paths) = &root_paths {
        if paths.is_empty() {
            return err(
                StatusCode::BAD_REQUEST,
                "library.invalid_roots",
                "媒体库至少需要一个目录",
            );
        }
    }
    let refs: Option<Vec<&str>> = root_paths
        .as_ref()
        .map(|paths| paths.iter().map(String::as_str).collect());
    let store = state.store.lock();
    let Some(current) = (match store.get_library(&id) {
        Ok(library) => library,
        Err(error) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            );
        }
    }) else {
        return missing_library();
    };
    let access_fields_sent = ["access_mode", "admin_visible", "member_ids"]
        .iter()
        .any(|field| body.get(*field).is_some());
    let access = access_fields_sent.then(|| {
        (
            body["access_mode"].as_str().unwrap_or(&current.access_mode),
            body["admin_visible"]
                .as_bool()
                .unwrap_or(current.admin_visible),
            if body.get("member_ids").is_some() {
                parse_member_ids(&body)
            } else {
                current.member_ids.clone()
            },
        )
    });
    if body.get("detect_intros").is_some() || body.get("enable_fingerprint").is_some() {
        let detect = body["detect_intros"]
            .as_bool()
            .unwrap_or(current.detect_intros);
        let enable_fp = body["enable_fingerprint"]
            .as_bool()
            .unwrap_or(current.enable_fingerprint);
        if let Err(error) = store.set_library_intro_settings(&id, detect, enable_fp) {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            );
        }
    }
    let switch_fields = [
        "realtime_watch",
        "generate_thumbnails",
        "extract_chapter_images",
        "exclude_from_home",
        "auto_series_collections",
    ];
    if switch_fields.iter().any(|field| body.get(*field).is_some()) {
        let rw = body.get("realtime_watch").and_then(|v| v.as_bool());
        let gt = body.get("generate_thumbnails").and_then(|v| v.as_bool());
        let ec = body.get("extract_chapter_images").and_then(|v| v.as_bool());
        let efh = body.get("exclude_from_home").and_then(|v| v.as_bool());
        let asc = body
            .get("auto_series_collections")
            .and_then(|v| v.as_bool());
        if let Err(error) = store.set_library_switch_settings(&id, rw, gt, ec, efh, asc) {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            );
        }
    }
    let match_rules = body["match_rules"].as_array().map(|arr| arr.clone());
    let default_filter_id_update: Option<Option<&str>> = if body.get("default_filter_id").is_some()
    {
        Some(body["default_filter_id"].as_str().filter(|s| !s.is_empty()))
    } else {
        None
    };
    match store.update_library_full(
        &id,
        name.as_deref(),
        refs.as_deref(),
        access
            .as_ref()
            .map(|(mode, visible, ids)| (*mode, *visible, ids.as_slice())),
        match_rules.as_deref(),
        default_filter_id_update,
    ) {
        Ok(library) => ok(library_json(&store, &library)).into_response(),
        Err(StoreError::Missing(_)) => missing_library(),
        Err(StoreError::Protected(message)) => {
            err(StatusCode::BAD_REQUEST, "library.protected", &message)
        }
        Err(error) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        ),
    }
}

/// DELETE /libraries/{id} — remove the library and its roots. The last library
/// of a kind is protected.
pub(crate) async fn delete_library(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Response {
    let preflight = state.library_work.freeze(&state.store, &id);
    match preflight {
        Ok(operation) => match crate::library_lifecycle::launch_library_removal(
            &state, operation, tokio::runtime::Handle::current()) {
            Ok(operation) => (StatusCode::ACCEPTED, ok(json!(operation))).into_response(),
            Err(error) => err(StatusCode::INTERNAL_SERVER_ERROR, "library.removal", &error),
        },
        Err(StoreError::Missing(_)) => missing_library(),
        Err(StoreError::Protected(message)) => err(StatusCode::CONFLICT, "library.protected", &message),
        Err(error) => err(StatusCode::INTERNAL_SERVER_ERROR, "store.error", &error.to_string()),
    }
}

/// PUT /libraries/{id}/default — promote to the kind's default library.
pub(crate) async fn set_default_library(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Response {
    let store = state.store.lock();
    match store.set_default_library(&id) {
        Ok(library) => ok(library_json(&store, &library)).into_response(),
        Err(StoreError::Missing(_)) => missing_library(),
        Err(error) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        ),
    }
}

/// PUT /libraries/order — full display order of all libraries (`ids`).
pub(crate) async fn reorder_libraries(
    State(state): State<ApiState>,
    Json(body): Json<Value>,
) -> Response {
    let ids: Vec<String> = body["ids"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
    let store = state.store.lock();
    match store.reorder_libraries(&refs) {
        Ok(()) => ok(json!({ "reordered": refs.len() })).into_response(),
        Err(error) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        ),
    }
}

/// GET /libraries/routing-options — return TMDB genres, country region presets, and country names.
pub(crate) async fn routing_options(State(state): State<ApiState>) -> Response {
    let catalog = state.catalog.as_ref();
    let movie_genres: Vec<Value> = catalog
        .genres(MediaKind::Movie)
        .unwrap_or_default()
        .into_iter()
        .map(|(id, name)| json!({ "id": id, "label": name }))
        .collect();
    let tv_genres: Vec<Value> = catalog
        .genres(MediaKind::Tv)
        .unwrap_or_default()
        .into_iter()
        .map(|(id, name)| json!({ "id": id, "label": name }))
        .collect();

    let region_presets = json!([
        {
            "key": "chinese",
            "label": "华语（大陆/港台）",
            "countries": ["CN", "HK", "TW"]
        },
        {
            "key": "western",
            "label": "欧美",
            "countries": ["US", "GB", "FR", "DE", "CA", "AU", "IT", "ES"]
        },
        {
            "key": "east_asia",
            "label": "日韩",
            "countries": ["JP", "KR"]
        },
        {
            "key": "southeast_asia",
            "label": "东南亚",
            "countries": ["TH", "SG", "MY", "VN", "ID", "PH"]
        }
    ]);

    let country_names = json!({
        "CN": "中国大陆",
        "HK": "中国香港",
        "TW": "中国台湾",
        "JP": "日本",
        "KR": "韩国",
        "US": "美国",
        "GB": "英国",
        "FR": "法国",
        "DE": "德国",
        "CA": "加拿大",
        "AU": "澳大利亚",
        "IT": "意大利",
        "ES": "西班牙",
        "TH": "泰国",
        "SG": "新加坡",
        "MY": "马来西亚",
        "IN": "印度",
        "RU": "俄罗斯"
    });

    ok(json!({
        "movie_genres": movie_genres,
        "tv_genres": tv_genres,
        "region_presets": region_presets,
        "country_names": country_names
    }))
    .into_response()
}
