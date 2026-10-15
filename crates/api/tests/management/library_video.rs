//! "Other" (video) library kind: create, scan without identification, browse,
//! and the artwork obligations of a rescan (P2-1 / P2-2).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use api::catalog::{ArtworkCandidate, ArtworkCandidates, Catalog};
use api::{ApiState, PosterFetch, router};
use axum::http::StatusCode;
use domain::MediaKind;
use downloader::MemoryDownloader;
use media::CatalogHit;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::common::*;

fn app(tmp: &tempfile::TempDir) -> (axum::Router, ApiState) {
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let state = state(tmp.path(), fetcher, downloader);
    let app = router(state.clone());
    (app, state)
}

/// 目录身份固定指向一张海报：Video 的封面因此走 catalog 通道，不依赖 ffmpeg。
struct ArtworkCatalog;

impl Catalog for ArtworkCatalog {
    fn search_movie(&self, _: &str) -> Result<Vec<CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn search_tv(&self, _: &str) -> Result<Vec<CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn popular_movie(&self) -> Result<Vec<CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn popular_tv(&self) -> Result<Vec<CatalogHit>, String> {
        Ok(Vec::new())
    }
    fn details(&self, _: MediaKind, _: &str) -> Result<Option<domain::Media>, String> {
        Ok(None)
    }
    fn poster_url(&self, _: MediaKind, _: &str) -> Result<Option<String>, String> {
        Ok(Some("https://image.tmdb.org/t/p/w342/clip-poster.jpg".into()))
    }
    fn image_candidates(&self, _: MediaKind, _: &str) -> Result<ArtworkCandidates, String> {
        Ok(ArtworkCandidates {
            posters: vec![ArtworkCandidate {
                file_path: "/clip-poster.jpg".into(),
                width: 2000,
                height: 3000,
                language: None,
            }],
            backdrops: vec![ArtworkCandidate {
                file_path: "/clip-backdrop.jpg".into(),
                width: 3840,
                height: 2160,
                language: None,
            }],
        })
    }
}

struct OfflinePosterFetch;

impl PosterFetch for OfflinePosterFetch {
    fn get(&self, _: &str) -> Result<Vec<u8>, String> {
        Ok(b"poster-image-bytes".to_vec())
    }
}

async fn put_scrape_setting(app: &axum::Router, mirror_images: bool) {
    let response = app
        .clone()
        .oneshot(request(
            "PUT",
            "/api/v1/settings/scrape",
            Some("management-secret"),
            json!({ "setting": { "mirror_images": mirror_images } }),
        ))
        .await
        .unwrap();
    let body = json_body(response).await;
    assert_eq!(body["data"]["effective"]["mirror_images"], mirror_images);
}

async fn create_video_library(app: &axum::Router, root: &std::path::Path) -> String {
    let created = app
        .clone()
        .oneshot(request(
            "POST",
            "/api/v1/libraries",
            Some("management-secret"),
            json!({
                "name": "家庭录像",
                "kind": "video",
                "root_paths": [root.display().to_string()],
            }),
        ))
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);
    json_body(created).await["data"]["id"]
        .as_str()
        .unwrap()
        .to_string()
}

struct VideoArtworkFixture {
    _tmp: tempfile::TempDir,
    app: axum::Router,
    state: ApiState,
    library_id: String,
    root: PathBuf,
    clip: PathBuf,
}

fn media_id_for(state: &ApiState, path: &std::path::Path) -> domain::MediaId {
    let store = state.store();
    let guard = store.lock();
    guard
        .ledger_by_path(&path.display().to_string())
        .unwrap()
        .expect("path must be ledgered")
        .media_id
}

/// 扫描一次（镜像关闭）→ 打开镜像并给已入账 Media 一个目录身份。
///
/// 此时磁盘上的文件**没有任何变化**：这正是 P2-1 回归的用户场景。
async fn video_artwork_fixture() -> VideoArtworkFixture {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("homevideos");
    std::fs::create_dir_all(&root).unwrap();
    let clip = root.join("clip.mp4");
    std::fs::write(&clip, b"video").unwrap();

    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    let state = state(tmp.path(), fetcher, downloader)
        .with_catalog(Arc::new(ArtworkCatalog))
        .with_poster_fetch(Arc::new(OfflinePosterFetch));
    let app = router(state.clone());

    let library_id = create_video_library(&app, &root).await;
    put_scrape_setting(&app, false).await;
    scan_and_wait(&app, &library_id).await;
    assert!(
        !root.join("poster.jpg").exists(),
        "镜像关闭时不得写封面"
    );

    put_scrape_setting(&app, true).await;
    {
        let store = state.store();
        let guard = store.lock();
        let row = guard
            .ledger_by_path(&clip.display().to_string())
            .unwrap()
            .expect("clip 必须已入账");
        let mut media = guard.get_media(row.media_id).unwrap().expect("media");
        media.tmdb_id = Some("603".into());
        guard.update_media(&media).unwrap();
    }
    VideoArtworkFixture {
        _tmp: tmp,
        app,
        state,
        library_id,
        root,
        clip,
    }
}

#[tokio::test]
async fn video_library_scan_browses_by_filename() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _state) = app(&tmp);
    let root = tmp.path().join("homevideos");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("宝宝满月纪念.mp4"), b"video").unwrap();

    let id = create_video_library(&app, &root).await;

    scan_and_wait(&app, &id).await;

    let items = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/libraries/{id}/items"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    let list = items["data"].as_array().unwrap();
    assert_eq!(list.len(), 1, "each video file becomes one item");
    assert_eq!(list[0]["title"], "宝宝满月纪念");
    assert_eq!(list[0]["kind"], "video");

    let detail = json_body(
        app.clone()
            .oneshot(request(
                "GET",
                &format!(
                    "/api/v1/libraries/{id}/items/{}",
                    list[0]["media_item_id"].as_str().unwrap()
                ),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        detail["data"]["files"][0]["path"],
        root.join("宝宝满月纪念.mp4").display().to_string()
    );
}

/// P2-1：镜像图片在第一次扫描之后才打开时，未变化的文件也必须在下一轮扫描补封面。
#[tokio::test]
async fn rescan_backfills_video_artwork_for_unchanged_files() {
    let fix = video_artwork_fixture().await;
    let report = scan_and_wait(&fix.app, &fix.library_id).await;
    assert_eq!(
        std::fs::read(fix.root.join("poster.jpg")).unwrap(),
        b"poster-image-bytes",
        "未变化的 Video 文件在新一轮扫描里必须重试封面: {report}"
    );
}

/// P2-1：用户锁定的海报即使被重扫命中也不得被覆盖。
#[tokio::test]
async fn rescan_keeps_a_locked_video_poster() {
    let fix = video_artwork_fixture().await;
    let poster = fix.root.join("poster.jpg");
    scan_and_wait(&fix.app, &fix.library_id).await;
    assert_eq!(std::fs::read(&poster).unwrap(), b"poster-image-bytes");

    let media_id = media_id_for(&fix.state, &fix.clip);
    {
        let store = fix.state.store();
        let guard = store.lock();
        guard
            .put_setting(
                &format!("artwork.{media_id}"),
                &json!({ "poster_locked": true }).to_string(),
            )
            .unwrap();
    }
    std::fs::write(&poster, b"user-poster").unwrap();

    scan_and_wait(&fix.app, &fix.library_id).await;
    assert_eq!(
        std::fs::read(&poster).unwrap(),
        b"user-poster",
        "锁定海报不得被扫描覆盖"
    );
}

/// P2-2：扫描收尾仍要清掉「可重试」的封面标记，且不得清掉已发布的标记。
#[tokio::test]
async fn video_scan_clears_only_the_retryable_cover_marker() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, state) = app(&tmp);
    let root = tmp.path().join("homevideos");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("clip.mp4"), b"video").unwrap();
    let id = create_video_library(&app, &root).await;
    let key = format!("library_cover_checked.{id}");
    let set_marker = |value: &str| {
        state.store().lock().put_setting(&key, value).unwrap();
    };
    let marker = || state.store().lock().get_setting(&key).unwrap();

    set_marker("auto-v2");
    scan_and_wait(&app, &id).await;
    assert_ne!(
        marker().as_deref(),
        Some("auto-v2"),
        "扫描必须清掉可重试的封面标记"
    );

    set_marker("checked-v2");
    scan_and_wait(&app, &id).await;
    assert_eq!(
        marker().as_deref(),
        Some("checked-v2"),
        "已发布的封面标记不得被扫描清掉"
    );
}
