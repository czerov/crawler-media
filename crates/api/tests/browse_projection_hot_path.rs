//! P2-5：浏览投影接生产读路径的 API 级证据。
//!
//! 覆盖：投影输出与旧全量读取语义等价；热请求零 NFO/stat；无关 ledger/catalog
//! 增长不增加目标请求的文件 IO；scope 变化不返回旧授权事实。

#[path = "support/mod.rs"]
mod support;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use api::ApiState;
use api::browse_file_read::BrowseFileRead;
use api::browse_projection::ProjectionService;
use api::router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use domain::{Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource, UserId};
use serde_json::Value;
use support::library_presence::PresenceFixture;
use tower::ServiceExt;

/// 计数浏览文件 seam：投影热路径必须让它保持 0。
#[derive(Default)]
struct CountingFiles {
    metadata: AtomicUsize,
    read_nfo: AtomicUsize,
    is_file: AtomicUsize,
}

impl CountingFiles {
    fn total(&self) -> usize {
        self.metadata.load(Ordering::SeqCst)
            + self.read_nfo.load(Ordering::SeqCst)
            + self.is_file.load(Ordering::SeqCst)
    }

    fn reset(&self) {
        self.metadata.store(0, Ordering::SeqCst);
        self.read_nfo.store(0, Ordering::SeqCst);
        self.is_file.store(0, Ordering::SeqCst);
    }
}

impl BrowseFileRead for CountingFiles {
    fn read_nfo(&self, path: &Path) -> Option<library::NfoMeta> {
        self.read_nfo.fetch_add(1, Ordering::SeqCst);
        library::read_nfo(path)
    }

    fn metadata(&self, path: &Path) -> std::io::Result<std::fs::Metadata> {
        self.metadata.fetch_add(1, Ordering::SeqCst);
        std::fs::metadata(path)
    }

    fn is_file(&self, path: &Path) -> bool {
        self.is_file.fetch_add(1, Ordering::SeqCst);
        path.is_file()
    }
}

/// 真视频 + 可选 NFO/海报 + ledger 行，全部挂在 Library 根下。
fn seed_movie(
    fix: &PresenceFixture,
    title: &str,
    tmdb_id: Option<&str>,
    nfo: Option<&str>,
    poster: bool,
) -> MediaId {
    let media_id = MediaId::new();
    let dir = fix.root.join(title);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{title}.mkv"));
    std::fs::write(&file, b"video-bytes").unwrap();
    if let Some(body) = nfo {
        std::fs::write(dir.join(format!("{title}.nfo")), body).unwrap();
    }
    if poster {
        std::fs::write(dir.join("poster.jpg"), b"poster").unwrap();
    }
    let store = fix.state.store();
    let guard = store.lock();
    guard
        .insert_media(&Media {
            id: media_id,
            kind: MediaKind::Movie,
            title: title.into(),
            year: Some(2024),
            original_title: Some(title.into()),
            tmdb_id: tmdb_id.map(str::to_string),
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    guard
        .insert_ledger(&LedgerRow {
            id: LedgerId::new(),
            media_id,
            path: file.display().to_string(),
            season: None,
            episode: None,
            resolution: Some("1080p".into()),
            codec: Some("H264".into()),
            hdr: None,
            quality_source: QualitySource::Probe,
            confidence: Confidence::High,
            filter_score: None,
        })
        .unwrap();
    media_id
}

/// 一条指向不存在文件的 ledger 行，随后由 `verify_library_files` 标 missing。
fn seed_missing_movie(fix: &PresenceFixture, title: &str) -> MediaId {
    let media_id = MediaId::new();
    let path = fix.root.join(title).join(format!("{title}.mkv"));
    let store = fix.state.store();
    let guard = store.lock();
    guard
        .insert_media(&Media {
            id: media_id,
            kind: MediaKind::Movie,
            title: title.into(),
            year: Some(2021),
            original_title: None,
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    guard
        .insert_ledger(&LedgerRow {
            id: LedgerId::new(),
            media_id,
            path: path.display().to_string(),
            season: None,
            episode: None,
            resolution: None,
            codec: None,
            hdr: None,
            quality_source: QualitySource::Release,
            confidence: Confidence::High,
            filter_score: None,
        })
        .unwrap();
    let roots = vec![fix.root.clone()];
    guard.verify_library_files(&roots, 1_700_000_000_000).unwrap();
    media_id
}

async fn get(state: &ApiState, url: &str, token: &str) -> Value {
    let app = router(state.clone());
    let request = Request::builder()
        .method("GET")
        .uri(url)
        .header("Authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK, "{url}");
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn seeded_fixture() -> PresenceFixture {
    let fix = PresenceFixture::new();
    seed_movie(
        &fix,
        "Alpha",
        None,
        Some(
            "<movie><rating>8.5</rating><runtime>150</runtime><original_language>en</original_language><genre>Action</genre></movie>",
        ),
        true,
    );
    seed_movie(
        &fix,
        "Beta",
        None,
        Some("<movie><rating>5.5</rating><runtime>90</runtime><original_language>ja</original_language></movie>"),
        false,
    );
    // catalog 补充：无 NFO，只有 tmdb 缓存体（scraped=true）。
    let delta = seed_movie(&fix, "Delta", Some("424242"), None, true);
    {
        let store = fix.state.store();
        let guard = store.lock();
        guard
            .put_catalog_cache(
                "tmdb",
                "/movie/424242?language=zh-CN",
                "{\"vote_average\":7.5,\"runtime\":101,\"original_language\":\"fr\",\"genres\":[{\"id\":18}],\"origin_country\":[\"FR\"]}",
                1000,
                None,
            )
            .unwrap();
        // 同一详情、同一 fetched_at 的第二个 language：定向读取与全量读取必须
        // 用同一个稳定并列键选中同一行（P2-NEW-2）。
        guard
            .put_catalog_cache(
                "tmdb",
                "/movie/424242?language=en-US",
                "{\"vote_average\":9.9,\"runtime\":88,\"original_language\":\"en\",\"genres\":[{\"id\":35}],\"origin_country\":[\"US\"]}",
                1000,
                None,
            )
            .unwrap();
    }
    let _ = delta;
    seed_missing_movie(&fix, "Echo");
    fix
}

const COMPARE_URLS: &[&str] = &[
    "items",
    "items?sort=rating&order=desc",
    "items?sort=added_at&order=asc",
    "items?sort=size&order=desc",
    "items?sort=release_date&order=desc",
    "items?stock=unscraped",
    "items?g=28",
    "items?limit=2&offset=1",
    "gallery",
    "gallery?limit=1",
    "item-index",
    "facets",
    "facets?tier=primary",
];

#[tokio::test]
async fn projected_browse_output_matches_full_read_on_the_same_fixture() {
    let fix = seeded_fixture();
    let projected = fix
        .state
        .clone()
        .with_projection_service(Arc::new(ProjectionService::new()));
    let live = fix
        .state
        .clone()
        .with_projection_service(Arc::new(ProjectionService::disabled()));

    for suffix in COMPARE_URLS {
        let url = format!("/api/v1/libraries/{}/{}", fix.library_id, suffix);
        let projected_body = get(&projected, &url, &fix.admin_token).await;
        let live_body = get(&live, &url, &fix.admin_token).await;
        assert_eq!(
            projected_body, live_body,
            "投影输出必须与旧全量读取逐字段一致：{url}"
        );
    }
    assert!(
        projected
            .store()
            .lock()
            .browse_projection(&fix.library_id)
            .unwrap()
            .is_some(),
        "等价比较必须有已发布的投影，否则比较是空转"
    );
}

/// `mark_dirty` 必须让下一次请求重建投影：外部新增图片（不递增 revision）可见。
#[tokio::test]
async fn mark_dirty_rebuilds_projection_after_external_file_change() {
    let fix = PresenceFixture::new();
    let media_id = seed_movie(&fix, "NoArt", None, None, false);
    let state = fix
        .state
        .clone()
        .with_projection_service(Arc::new(ProjectionService::new()));
    let url = format!("/api/v1/libraries/{}/items", fix.library_id);

    let before = get(&state, &url, &fix.admin_token).await;
    assert!(
        before["data"][0]["poster_url"].is_null(),
        "无图片时 poster_url 必须为 null"
    );

    let poster = fix.root.join("NoArt").join("poster.jpg");
    std::fs::write(&poster, b"poster").unwrap();
    state.projection.mark_dirty(&fix.library_id);
    assert!(state.projection.is_dirty(&fix.library_id));

    let after = get(&state, &url, &fix.admin_token).await;
    assert!(
        after["data"][0]["poster_url"]
            .as_str()
            .is_some_and(|value| value.starts_with("/posters/")),
        "mark_dirty 后必须重建并看到新图片：{after}"
    );
    let _ = media_id;
}

#[tokio::test]
async fn hot_browse_requests_do_zero_file_io() {
    let fix = seeded_fixture();
    let files = Arc::new(CountingFiles::default());
    let state = fix
        .state
        .clone()
        .with_browse_file_read(files.clone())
        .with_projection_service(Arc::new(ProjectionService::new()));

    // 预热：构建投影本身允许做文件 IO（首次请求的允许成本）。
    state
        .projection
        .ensure(state.clone(), fix.library_id.clone())
        .await
        .expect("projection build");

    for suffix in ["items", "gallery", "facets", "item-index"] {
        files.reset();
        let url = format!("/api/v1/libraries/{}/{}", fix.library_id, suffix);
        let _ = get(&state, &url, &fix.admin_token).await;
        assert_eq!(
            files.total(),
            0,
            "热浏览请求 {suffix} 不得做任何 NFO/stat/is_file"
        );
    }

    // 反证计数 seam 有效：同一请求走旧全量路径时必然有文件 IO。
    let live = state
        .clone()
        .with_projection_service(Arc::new(ProjectionService::disabled()));
    files.reset();
    let _ = get(
        &live,
        &format!("/api/v1/libraries/{}/items", fix.library_id),
        &fix.admin_token,
    )
    .await;
    assert!(
        files.total() > 0,
        "旧全量路径必须有文件 IO（否则计数 seam 失效）"
    );
}

/// P2-NEW-4：用户 `playback_units` 心跳只影响用户派生观看状态，不得让公共
/// 图片/NFO 投影失效；热浏览仍必须零文件 IO。
#[tokio::test]
async fn playback_unit_write_does_not_invalidate_public_projection() {
    let fix = PresenceFixture::new();
    let media_id = seed_movie(&fix, "Gamma", None, None, true);
    let files = Arc::new(CountingFiles::default());
    let state = fix
        .state
        .clone()
        .with_browse_file_read(files.clone())
        .with_projection_service(Arc::new(ProjectionService::new()));

    // 预热公共投影（首次构建允许文件 IO）。
    state
        .projection
        .ensure(state.clone(), fix.library_id.clone())
        .await
        .expect("projection build");
    assert!(!state.projection.is_dirty(&fix.library_id));
    let before = fix
        .state
        .store()
        .lock()
        .browse_projection(&fix.library_id)
        .unwrap()
        .expect("warm projection");
    // 让重建（若发生）落在不同的毫秒，refreshed_at 才能区分「重建」与「未重建」。
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;

    // 另一个用户的播放心跳：只写 playback_units（subscribe revision 递增）。
    let user = UserId::new();
    fix.state
        .store()
        .lock()
        .set_playback_state(user, media_id, 5_000, Some(true), Some(true), None, None)
        .unwrap();
    assert!(
        !state.projection.is_dirty(&fix.library_id),
        "播放写入不得标脏公共投影"
    );

    // 热浏览：不得重跑 NFO/stat。
    files.reset();
    let url = format!("/api/v1/libraries/{}/items", fix.library_id);
    let _ = get(&state, &url, &fix.admin_token).await;
    assert_eq!(
        files.total(),
        0,
        "playback_units 写入后热浏览仍必须零文件 IO"
    );

    // 公共投影不得重建（refreshed_at 是发布时刻；重建会晚于 5ms 的间隔）。
    let after = fix
        .state
        .store()
        .lock()
        .browse_projection(&fix.library_id)
        .unwrap()
        .expect("projection still published");
    assert_eq!(
        before.refreshed_at_ms, after.refreshed_at_ms,
        "playback_units 写入后公共投影不得重建"
    );
}

/// 无关 ledger / catalog 增长不得让目标 Library 的请求或重建做更多文件 IO。
#[tokio::test]
async fn unrelated_ledger_and_catalog_growth_does_not_add_target_file_io() {
    let fix = seeded_fixture();
    let files = Arc::new(CountingFiles::default());
    let state = fix
        .state
        .clone()
        .with_browse_file_read(files.clone())
        .with_projection_service(Arc::new(ProjectionService::new()));

    files.reset();
    state
        .projection
        .ensure(state.clone(), fix.library_id.clone())
        .await
        .unwrap();
    let baseline_build_io = files.total();
    assert!(baseline_build_io > 0, "构建投影必须观察目标文件");

    // 另一个 Library 的 200 条 ledger + 200 个无关 catalog body。
    let other_root = fix.dir.path().join("other-movies");
    std::fs::create_dir_all(&other_root).unwrap();
    {
        let store = fix.state.store();
        let guard = store.lock();
        guard
            .create_library(
                MediaKind::Movie,
                "Other",
                &[other_root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        for index in 0..200 {
            let media_id = MediaId::new();
            guard
                .insert_media(&Media {
                    id: media_id,
                    kind: MediaKind::Movie,
                    title: format!("Other {index}"),
                    year: Some(2000),
                    original_title: None,
                    tmdb_id: None,
                    douban_id: None,
                    tvdb_id: None,
                    bangumi_id: None,
                    anilist_id: None,
                })
                .unwrap();
            guard
                .insert_ledger(&LedgerRow {
                    id: LedgerId::new(),
                    media_id,
                    path: other_root
                        .join(format!("Other {index}.mkv"))
                        .display()
                        .to_string(),
                    season: None,
                    episode: None,
                    resolution: None,
                    codec: None,
                    hdr: None,
                    quality_source: QualitySource::Release,
                    confidence: Confidence::High,
                    filter_score: None,
                })
                .unwrap();
            guard
                .put_catalog_cache(
                    "tmdb",
                    &format!("/movie/{}", 900_000 + index),
                    "{\"vote_average\":1.0}",
                    2000 + index,
                    None,
                )
                .unwrap();
        }
    }

    // 重建：无关行只参与 scope 过滤（CPU），不得触发额外文件 IO。
    files.reset();
    state
        .projection
        .ensure(state.clone(), fix.library_id.clone())
        .await
        .unwrap();
    assert_eq!(
        files.total(),
        baseline_build_io,
        "无关 ledger/catalog 增长不得增加目标 Library 的构建文件 IO"
    );

    for suffix in ["items", "gallery", "facets"] {
        files.reset();
        let url = format!("/api/v1/libraries/{}/{}", fix.library_id, suffix);
        let _ = get(&state, &url, &fix.admin_token).await;
        assert_eq!(files.total(), 0, "热请求 {suffix} 仍必须零文件 IO");
    }
}

/// scope 变化后不得返回旧代事实：预热父库投影后再建子库，父库不得吞掉子库行。
#[tokio::test]
async fn scope_change_after_warm_projection_never_serves_stale_rows() {
    let fix = seeded_fixture();
    let state = fix
        .state
        .clone()
        .with_projection_service(Arc::new(ProjectionService::new()));
    state
        .projection
        .ensure(state.clone(), fix.library_id.clone())
        .await
        .unwrap();

    // 同一个 Media 在子库根下再放一份文件与 ledger 行。
    let shared = {
        let store = fix.state.store();
        let guard = store.lock();
        guard
            .list_ledger()
            .unwrap()
            .into_iter()
            .find(|row| row.path.contains("Alpha"))
            .unwrap()
    };
    let child_root = fix.root.join("child");
    std::fs::create_dir_all(&child_root).unwrap();
    let child = {
        let store = fix.state.store();
        let guard = store.lock();
        guard
            .create_library(
                MediaKind::Movie,
                "child",
                &[child_root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap()
    };
    let child_file = child_root.join("Alpha").join("Alpha.mkv");
    std::fs::create_dir_all(child_file.parent().unwrap()).unwrap();
    std::fs::write(&child_file, b"video-bytes").unwrap();
    {
        let store = fix.state.store();
        let guard = store.lock();
        guard
            .insert_ledger(&LedgerRow {
                id: LedgerId::new(),
                path: child_file.display().to_string(),
                ..shared.clone()
            })
            .unwrap();
    }

    let parent_items = get(
        &state,
        &format!("/api/v1/libraries/{}/items", fix.library_id),
        &fix.admin_token,
    )
    .await;
    let alpha = parent_items["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["title"] == "Alpha")
        .expect("Alpha still owned by parent");
    assert_eq!(
        alpha["file_count"], 1,
        "父库不得把预热后才出现的子库行算进旧代"
    );

    let child_items = get(
        &state,
        &format!("/api/v1/libraries/{}/items", child.id),
        &fix.admin_token,
    )
    .await;
    let child_alpha = child_items["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["title"] == "Alpha")
        .expect("child owns its row");
    assert_eq!(child_alpha["file_count"], 1);
}
