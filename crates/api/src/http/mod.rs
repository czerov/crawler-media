//! Self-hosted REST API (`/api/v1`), contract in
//! `docs/api-contracts/self.md`. No upstream compatibility: UUID ids,
//! unified `{ok, data}` / `{ok, error}` envelope, domain-shaped DTOs.
//!
//! Module layout mirrors the contract resources:
//! `auth` / `users` / `sites` / `search` / `rule_sets` / `subscriptions` /
//! `downloaders` / `library` / `jobs` / `directory` / `notify` / `playback`.

pub(crate) mod auth;
pub(crate) mod browser_settings;
pub(crate) mod collections;
pub(crate) mod directory;
pub(crate) mod discover;
pub(crate) mod downloaders;
pub(crate) mod file_delete;
pub(crate) mod fs;
pub(crate) mod image_proxy;
pub(crate) mod job_maintenance;
pub(crate) mod jobs;
pub(crate) mod library;
pub(crate) mod library_admin;
pub(crate) mod library_artwork;
pub(crate) mod library_chapters;
pub(crate) mod library_config;
pub(crate) mod library_delete;
pub(crate) mod library_deletion;
pub(crate) mod library_duplicates;
pub(crate) mod library_gallery;
pub(crate) mod library_housekeeping;
pub(crate) mod library_organize;
pub(crate) mod library_presence;
pub(crate) mod library_scan;
pub(crate) mod media;
pub(crate) mod media_posters;
pub(crate) mod media_visibility;
pub(crate) mod notify;
pub(crate) mod playback;
pub(crate) mod playback_activity;
pub(crate) mod playback_device_target;
pub(crate) mod playback_logs;
pub(crate) mod playback_stats;
pub(crate) mod playback_views;
pub(crate) mod proxy_settings;
pub(crate) mod reidentify;
pub(crate) mod request_observation;
pub(crate) mod routing_preview;
pub(crate) mod rule_sets;
pub(crate) mod scrape_settings;
pub(crate) mod search;
pub(crate) mod settings;
pub(crate) mod sites;
pub(crate) mod subscribe_schedule;
pub(crate) mod subscription_depth;
pub(crate) mod subscription_missing;
pub(crate) mod subscriptions;
pub(crate) mod system_logs;
pub(crate) mod title_ref;
pub(crate) mod users;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post, put};
use axum::{Json, Router};
use serde_json::{Value, json};

use crate::management::ApiState;

/// Success envelope: `{"ok": true, "data": ...}`.
/// 管理员授权在路由层收口：admin Router 统一挂 `auth::require_admin`
/// 中间件（authenticate 之后运行），成员请求在进入 handler 前就被 403，
/// 不再依赖每个 handler 自己记得调用检查。

pub(crate) fn ok(data: Value) -> Json<Value> {
    Json(json!({ "ok": true, "data": data }))
}

/// Success envelope for a plain list payload.
pub(crate) fn ok_list(data: Vec<Value>) -> Json<Value> {
    ok(Value::Array(data))
}

/// Error envelope: `{"ok": false, "error": {"code", "message"}}`.
pub(crate) fn err(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(json!({
            "ok": false,
            "error": { "code": code, "message": message },
        })),
    )
        .into_response()
}

pub(crate) async fn health() -> Response {
    ok(json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "commit": env!("GIT_HASH"),
    }))
    .into_response()
}

pub(crate) fn router(state: ApiState) -> Router {
    // 生产路径：阈值在启动时从 `ServerConfig` 读一次，再作为 layer state 注入。
    router_with_slow_after(
        state,
        std::time::Duration::from_millis(crate::config::slow_request_ms()),
    )
}

/// Router with an injected slow-request threshold. Tests pass an explicit value
/// instead of mutating the process environment (which would race).
pub fn router_with_slow_after(state: ApiState, slow_after: std::time::Duration) -> Router {
    let public = public_routes();
    let member = member_routes();
    let admin = admin_routes(&state);

    let protected = member
        .merge(admin)
        .route_layer(axum::middleware::from_fn_with_state(
            state.store.clone(),
            auth::authenticate,
        ));

    let base = public.merge(protected).with_state(state);
    base.layer(axum::middleware::from_fn_with_state(
        slow_after,
        request_logger,
    ))
    .layer(axum::extract::DefaultBodyLimit::max(20 * 1024 * 1024))
}

fn public_routes() -> Router<ApiState> {
    Router::new()
        .route("/auth/login", post(auth::login))
        .route("/auth/logout", post(auth::logout))
        .route("/health", get(health))
        .route("/images/proxy", get(image_proxy::proxy_image))
        // 图片资源（海报/背板/剧照/章节图/媒体库封面）走 <img> 加载，无法携带
        // Authorization 头（前端使用 Bearer token，不依赖 session cookie）——必须公开，
        // 否则封面与剧照永远 401 占位。
        .route("/media/{id}/poster", get(media_posters::get_media_poster))
        .route("/libraries/{id}/cover", get(library_artwork::library_cover))
        .route("/posters/{ledger_id}", get(library_artwork::poster))
        .route("/fanart/{ledger_id}", get(library_artwork::fanart))
        .route("/stills/{ledger_id}", get(library_artwork::stills))
        .route(
            "/chapters/{ledger_id}/{index}",
            get(library_chapters::chapter_image),
        )
}

fn member_routes() -> Router<ApiState> {
    Router::new()
        .merge(member_discovery_routes())
        .merge(member_subscriptions_routes())
        .merge(member_library_routes())
        .merge(member_playback_routes())
}

fn member_discovery_routes() -> Router<ApiState> {
    Router::new()
        // auth
        .route("/auth/me", get(auth::me))
        // sites（只读；变更在 admin 层）
        .route("/sites", get(sites::list_sites))
        .route("/sites/{id}", get(sites::get_site))
        .route("/sites/boost-stats", get(sites::boost_stats))
        .route("/sites/catalog", get(sites::site_catalog))
        // search
        .route("/search/torrents", get(search::search_torrents))
        .route("/search/torrents/stream", get(search::search_stream))
        .route("/search/titles", get(search::search_titles))
        .route("/search/library-items", get(search::search_library_items))
        .route(
            "/search/history",
            get(search::search_history).delete(search::clear_search_history_route),
        )
        .route(
            "/search/history/{id}",
            get(search::get_history_snapshot).delete(search::delete_history_entry_route),
        )
        .route(
            "/search/presets",
            get(search::get_presets).put(search::put_presets),
        )
        .route("/discover/{kind}", get(discover::discover_kind))
        .route(
            "/discover/{kind}/filtered",
            get(discover::discover_filtered),
        )
        .route(
            "/discover/{kind}/collection/{id}",
            get(discover::discover_collection),
        )
        .route("/genres/{kind}", get(discover::genres))
        .route("/media/douban/{id}", get(media::douban_detail))
        .route("/media/{kind}/{id}", get(media::media_detail))
        .route("/media/tv/{id}/season/{season}", get(media::season_detail))
        .route("/media/person/{id}", get(media::person_detail))
        // rule-sets (Filter groups)
        .route("/rule-sets", get(rule_sets::list_rule_sets))
        .route("/rule-sets/{id}", get(rule_sets::get_rule_set))
}

fn member_subscriptions_routes() -> Router<ApiState> {
    Router::new()
        // subscriptions（实例级归属在 handler 内校验）
        .route(
            "/subscriptions",
            get(subscriptions::list_subscriptions).post(subscriptions::create_subscription),
        )
        .route(
            "/subscriptions/{id}",
            get(subscriptions::get_subscription)
                .patch(subscriptions::patch_subscription)
                .delete(subscriptions::delete_subscription),
        )
        .route(
            "/subscriptions/{id}/run",
            post(crate::management::run_subscribe),
        )
        .route(
            "/subscriptions/{id}/search",
            post(subscriptions::run_subscription_search),
        )
        .route(
            "/subscriptions/{id}/upgrade-runs",
            post(subscription_depth::run_upgrade),
        )
        .route(
            "/subscriptions/{id}/missing-resource-searches",
            post(subscription_missing::run_missing_search),
        )
        .route(
            "/subscriptions/{id}/removal-preview",
            get(subscription_depth::removal_preview),
        )
        .route(
            "/subscriptions/{id}/season-cleanup",
            post(subscription_depth::season_cleanup),
        )
        .route(
            "/subscriptions/{id}/activities",
            get(subscription_depth::activities),
        )
        .route(
            "/subscriptions/{id}/release-forecast",
            get(subscriptions::release_forecast),
        )
        .route(
            "/subscriptions/today-arrivals",
            get(subscription_depth::today_arrivals),
        )
        .route(
            "/subscriptions/automation-readiness",
            get(subscriptions::automation_readiness),
        )
        .route("/subscriptions/title-preview", post(title_ref::preview))
        .route(
            "/subscriptions/download-routing-preview",
            post(routing_preview::preview),
        )
        // downloaders（只读 + 投递；变更在 admin 层）
        .route("/downloaders", get(downloaders::list_downloaders))
        .route("/downloaders/{id}", get(downloaders::get_downloader))
        .route("/downloaders/tasks", get(downloaders::list_tasks))
        .route("/downloaders/submit", post(downloaders::submit))
        .route("/downloaders/target-prefs", get(downloaders::target_prefs))
}

fn member_library_routes() -> Router<ApiState> {
    Router::new()
        // library + media（读取 / 观影操作；实体增删改在 admin 层）
        .route("/libraries", get(library::list_libraries))
        .route(
            "/collections",
            get(collections::list_collections).post(collections::create_collection),
        )
        .route(
            "/collections/{id}",
            axum::routing::patch(collections::rename_collection)
                .delete(collections::delete_collection),
        )
        .route(
            "/collections/{id}/items",
            get(collections::collection_items).post(collections::add_collection_item),
        )
        .route(
            "/collections/{id}/items/{media_item_id}",
            delete(collections::remove_collection_item),
        )
        .route(
            "/collections/{id}/order",
            put(collections::reorder_collection),
        )
        .route(
            "/collections/{id}/gallery",
            get(collections::collection_gallery),
        )
        .route("/libraries/{id}/items", get(library::list_items))
        .route("/libraries/{id}/items/{item_id}", get(library::item_detail))
        .route(
            "/libraries/{id}/items/{item_id}/episodes",
            get(library::item_episodes),
        )
        .route("/libraries/{id}/missing", get(library_scan::missing))
        .route("/libraries/{id}/item-index", get(library_scan::item_index))
        .route("/libraries/{id}/facets", get(library_scan::facets))
        .route("/libraries/{id}/gallery", get(library_gallery::gallery))
        .route(
            "/libraries/{id}/items/{item_id}/similar",
            get(library_organize::similar),
        )
        .route(
            "/libraries/{id}/items/{item_id}/chapters",
            get(library_chapters::chapters),
        )
        .route(
            "/libraries/{id}/items/{item_id}/artwork/candidates",
            get(library_artwork::artwork_candidates),
        )
        // catalog cache
        .route(
            "/catalog/cache",
            get(crate::catalog_cache::list_catalog_cache),
        )
        // jobs（读取；run/cancel/toggle/tick 位于 admin 层）
        .route("/jobs", get(jobs::list_jobs))
        .route("/jobs/{id}", get(jobs::get_job))
        .route("/jobs/stream", get(jobs::job_stream))
        // directory + settings + notify（读取；写入在 admin 层）
        .route("/directory", get(directory::get_directory))
        .route("/notify/config", get(notify::get_config))
        .route(
            "/ui/preferences",
            get(playback::get_ui_prefs).put(playback::put_ui_prefs),
        )
        .route(
            "/settings/scrape",
            get(scrape_settings::get_scrape_settings),
        )
        .route(
            "/settings/scrape/preview-naming",
            post(scrape_settings::preview_naming),
        )
        .route("/settings/languages", get(scrape_settings::languages))
        .route("/settings/countries", get(scrape_settings::countries))
}

fn member_playback_routes() -> Router<ApiState> {
    Router::new()
        // playback views (read playback_progress + ledger)
        .route("/playback/decide", post(playback::decide))
        .route("/playback/sessions", post(playback::start_session))
        .route("/playback/chapters", get(playback::cached_chapters))
        .route(
            "/playback/subtitles/{ledger_id}/{index}",
            get(playback::get_subtitle_file),
        )
        .route("/playback/progress", post(playback::progress))
        .route("/playback/metrics", post(playback::metrics))
        .route("/playback/policy", get(playback::get_policy))
        .route("/playback/items/{id}", get(playback::item))
        .route("/playback/items/{id}/episodes", get(playback::episodes))
        .route(
            "/playback/marks",
            get(playback::marks).post(playback::set_marks),
        )
        .route("/playback/resume", get(playback::resume))
        .route("/playback/client-log", post(playback::client_log))
        .route("/playback/up-next", get(playback_views::up_next))
        .route("/playback/favorites", get(playback_views::favorites))
        .route(
            "/playback/favorites/gallery",
            get(playback_views::favorites_gallery),
        )
        .route(
            "/playback/history",
            get(playback_logs::history).delete(playback_logs::clear_history),
        )
        .route("/playback/hardware", get(playback::hardware))
        .route("/playback/stats/watch", get(playback_stats::watch_stats))
        .route("/playback/activity", get(playback_activity::activity))
        .route(
            "/playback/activity/sessions/{device_id}/end",
            post(playback_activity::end_session),
        )
        .route("/playback/devices", get(playback_activity::devices))
        .route(
            "/playback/devices/{device_id}",
            delete(playback_activity::revoke_device),
        )
}

fn admin_routes(state: &ApiState) -> Router<ApiState> {
    Router::new()
        .merge(admin_core_routes())
        .merge(admin_library_routes())
        .merge(admin_metadata_routes())
        .merge(admin_ops_routes())
        .layer(axum::middleware::from_fn_with_state(
            state.store.clone(),
            auth::require_admin,
        ))
}

fn admin_core_routes() -> Router<ApiState> {
    Router::new()
        // users
        .route("/users", post(users::create_user).get(users::list_users))
        .route(
            "/users/{id}",
            patch(users::update_user).delete(users::delete_user),
        )
        // sites
        .route("/sites", post(sites::create_site))
        .route(
            "/sites/{id}",
            patch(sites::patch_site)
                .put(sites::put_site)
                .delete(sites::delete_site),
        )
        .route("/sites/{id}/verify", post(sites::verify_site))
        .route("/sites/{id}/login", post(sites::site_login))
        .route("/sites/{id}/check-in", post(sites::site_check_in))
        .route("/sites/{id}/boost", patch(sites::patch_boost))
        // rule-sets (Filter groups)
        .route("/rule-sets", post(rule_sets::create_rule_set))
        .route(
            "/rule-sets/{id}",
            patch(rule_sets::patch_rule_set).delete(rule_sets::delete_rule_set),
        )
        .route("/rule-sets/default", put(rule_sets::put_default_rule_set))
        // downloaders
        .route("/downloaders", post(downloaders::create_downloader))
        .route(
            "/downloaders/{id}",
            patch(downloaders::patch_downloader).delete(downloaders::delete_downloader),
        )
        .route(
            "/downloaders/{id}/verify",
            post(downloaders::verify_downloader),
        )
        .route(
            "/downloaders/target-prefs/{category}",
            put(downloaders::put_target_pref),
        )
        .route(
            "/downloaders/tasks/{hash}",
            delete(downloaders::delete_task),
        )
        .route(
            "/downloaders/tasks/{hash}/pause",
            post(downloaders::pause_task),
        )
        .route(
            "/downloaders/tasks/{hash}/resume",
            post(downloaders::resume_task),
        )
        .route("/downloaders/tasks/remove", post(downloaders::remove_task))
        .route(
            "/downloaders/tasks/replace",
            post(downloaders::replace_task),
        )
        .route(
            "/downloaders/limits",
            get(downloaders::get_limits).put(downloaders::set_limits),
        )
}

fn admin_library_routes() -> Router<ApiState> {
    Router::new()
        // libraries：实体增删改 + 条目/文件删除
        .route("/library-removals/{operation_id}", get(library_deletion::status))
        .route("/libraries", post(library_config::create_library))
        .route(
            "/libraries/{id}",
            patch(library_config::patch_library).delete(library_config::delete_library),
        )
        .route(
            "/libraries/{id}/default",
            put(library_config::set_default_library),
        )
        .route("/libraries/order", put(library_config::reorder_libraries))
        .route(
            "/libraries/routing-options",
            get(library_config::routing_options),
        )
        .route(
            "/libraries/{id}/items/{item_id}",
            delete(library_delete::delete_item),
        )
        .route(
            "/libraries/{id}/duplicates/{file_id}",
            delete(library_duplicates::delete_duplicate),
        )
        .route("/libraries/{id}/scan", post(library_scan::scan_library))
        .route(
            "/libraries/{id}/scan-operations/{operation_id}",
            get(library_scan::operations::get_scan_operation),
        )
        .route(
            "/libraries/{id}/scan-operations/{operation_id}/cancel",
            post(library_scan::operations::cancel_scan_operation),
        )
        .route("/libraries/{id}/verify", post(library_presence::verify_handler))
        .route(
            "/libraries/{id}/missing-rows",
            delete(library_presence::prune_handler),
        )
        .route(
            "/libraries/{id}/organize-preview",
            get(library_organize::preview_organize),
        )
        .route(
            "/libraries/{id}/path-reconciliation-preview",
            get(library_housekeeping::path_reconciliation_preview),
        )
        .route(
            "/libraries/{id}/root-consolidation-preview",
            get(library_housekeeping::root_consolidation_preview),
        )
        .route(
            "/libraries/{id}/items/{item_id}/reidentification-preview",
            get(reidentify::preview),
        )
        .route("/unidentified", get(library_admin::list_unidentified))
        .route(
            "/libraries/{id}/duplicates",
            get(library_duplicates::duplicates),
        )
        .route(
            "/libraries/{id}/path-reconciliations",
            post(library_housekeeping::path_reconciliations),
        )
        .route(
            "/libraries/{id}/root-consolidations",
            post(library_housekeeping::root_consolidations),
        )
        .route("/libraries/{id}/organize", post(library_organize::organize))
}

fn admin_metadata_routes() -> Router<ApiState> {
    Router::new()
        .route(
            "/libraries/{id}/metadata/refresh",
            post(library_scan::refresh_metadata),
        )
        .route(
            "/libraries/{id}/items/{item_id}/metadata/refresh",
            post(library_organize::refresh_item_metadata),
        )
        .route(
            "/libraries/{id}/items/{item_id}/probe",
            post(library::probe_item),
        )
        .route(
            "/libraries/{id}/items/relax-filter",
            post(library_organize::relax_filter),
        )
        .route("/reidentify", post(reidentify::reidentify))
        .route("/reidentify/extras", post(reidentify::extras))
        .route(
            "/libraries/{id}/items/{item_id}/chapters/generate",
            post(library_chapters::generate_chapters),
        )
        .route(
            "/libraries/{id}/items/{item_id}/chapters/refresh",
            post(library_chapters::refresh_item_chapters),
        )
        .route(
            "/libraries/{id}/items/{item_id}/probe-status",
            get(library_chapters::probe_status),
        )
        .route(
            "/libraries/{id}/cover",
            post(library_artwork::upload_library_cover)
                .delete(library_artwork::delete_library_cover),
        )
        .route(
            "/libraries/{id}/cover/generate",
            post(library_artwork::generate_library_cover),
        )
        .route(
            "/libraries/{id}/items/{item_id}/artwork/select",
            post(library_artwork::select_artwork),
        )
        .route(
            "/libraries/{id}/items/{item_id}/artwork/upload",
            post(library_artwork::upload_artwork),
        )
        .route(
            "/unidentified/{id}/claim",
            post(library_admin::claim_unidentified),
        )
        .route(
            "/unidentified/claim",
            post(crate::claim::claim_unidentified_v1),
        )
        .route(
            "/catalog/cache",
            delete(crate::catalog_cache::delete_catalog_cache),
        )
        .route("/fs/browse", get(fs::browse_fs))
        // ledger: 全局台账查看与删除是敏感文件操作，仅管理员可用
        .route("/ledger", get(library_admin::list_ledger))
        .route(
            "/ledger/{id}",
            axum::routing::delete(library_admin::delete_ledger_row),
        )
        .route(
            "/ledger/{id}/retransfer",
            post(library_admin::retransfer_ledger_row),
        )
}

fn admin_ops_routes() -> Router<ApiState> {
    Router::new()
        // jobs：任务执行与启停
        .route("/jobs/{id}", patch(jobs::toggle_job))
        .route("/jobs/{id}/schedule", put(jobs::update_job_schedule))
        // DB8 维护配置与只读预览（仅管理员；会员在 admin 中间件层 403）。
        .route(
            "/jobs/{id}/maintenance-config",
            put(job_maintenance::update_maintenance_config),
        )
        .route(
            "/jobs/{id}/maintenance-preview",
            post(job_maintenance::preview_maintenance),
        )
        .route("/jobs/{id}/run", post(jobs::run_job))
        .route("/jobs/{id}/cancel", post(jobs::cancel_job))
        .route("/jobs/tick", post(jobs::tick_jobs))
        .route("/search/admit", post(crate::admit::admit_torrent_v1))
        .route("/playback/policy", put(playback::put_policy))
        // directory + settings + notify（写入）
        .route("/directory", put(directory::put_directory))
        .route("/directory/roots", post(directory::add_root))
        .route("/directory/roots/{id}", delete(directory::delete_root))
        .route("/notify/config", put(notify::put_config))
        .route("/notify/test", post(notify::test))
        .route(
            "/settings/metadata",
            get(settings::get_metadata).put(settings::put_metadata),
        )
        .route("/settings/metadata/test", post(settings::test_metadata))
        .route(
            "/settings/proxy",
            get(settings::get_proxy).put(settings::put_proxy),
        )
        .route("/settings/proxy/test", post(settings::test_proxy))
        .route("/settings/proxy/diagnose", post(settings::diagnose_proxy))
        .route(
            "/settings/scrape",
            put(scrape_settings::put_scrape_settings),
        )
        // Browser settings/status expose the CDP endpoint, managed port and
        // binary path (a local CDP port can drive the browser): admin-only.
        .route(
            "/settings/browser",
            get(settings::get_browser_settings).put(settings::put_browser_settings),
        )
        .route(
            "/settings/browser/obscura",
            get(settings::get_obscura_status),
        )
        .route(
            "/settings/browser/obscura/start",
            post(settings::post_obscura_start),
        )
        .route(
            "/settings/browser/obscura/stop",
            post(settings::post_obscura_stop),
        )
        .route(
            "/settings/browser/sync-cdp",
            post(settings::sync_cdp_cookies),
        )
        // system logs (admin-only)
        .route("/system/logs", get(system_logs::list_logs))
        .route("/system/logs/stream", get(system_logs::stream_logs))
        .route("/system/logs/clear", post(system_logs::clear_logs))
        .route("/system/logs/export", get(system_logs::export_logs))
}

/// 请求观测中间件。
///
/// `headers_latency_ms` 只覆盖到**响应头就绪**为止，不包含 body / SSE / 视频流的
/// 完整传输时间。慢阈值由 layer state 注入，中间件本身不读 env。
pub async fn request_logger(
    axum::extract::State(slow_after): axum::extract::State<std::time::Duration>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let query = request
        .uri()
        .query()
        .map(request_observation::redact_query)
        .filter(|query| !query.is_empty());
    // `MatchedPath` 由路由层在匹配后写入 extensions；未匹配时退回 raw path。
    let route = request
        .extensions()
        .get::<axum::extract::MatchedPath>()
        .map(|matched| matched.as_str().to_string())
        .unwrap_or_else(|| path.clone());
    let request_id = request_observation::request_id_from(request.headers());
    let user_agent = request
        .headers()
        .get(axum::http::header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let start = std::time::Instant::now();

    let mut response = next.run(request).await;
    let elapsed = start.elapsed();
    let status = response.status();
    let observation = request_observation::observe(
        method.as_str(),
        &route,
        &request_id,
        status.as_u16(),
        elapsed,
        slow_after,
    );
    request_observation::log_request(
        &observation,
        &path,
        query.as_deref(),
        user_agent.as_deref(),
    );
    if let Ok(value) = axum::http::HeaderValue::from_str(&observation.request_id) {
        response.headers_mut().insert("x-request-id", value);
    }
    response
}
