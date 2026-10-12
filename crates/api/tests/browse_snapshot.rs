//! Library 浏览请求级快照回归。
//!
//! 关键不变量：NFO/stat 与过滤排序都在 Store 锁之外；归属仍按「最长唯一根」；
//! 空图组在 gallery 分页之前剔除。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};

use api::ApiState;
use api::browse_file_read::BrowseFileRead;
use api::router;
use axum::http::StatusCode;
use domain::{Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource};
use downloader::MemoryDownloader;
use parking_lot::Mutex as ParkingMutex;
use serde_json::Value;
use tower::ServiceExt;

#[path = "management/common.rs"]
mod common;
use common::{Fixtures, json_data, request, state};

struct Gate {
    entered: Mutex<bool>,
    entered_cv: Condvar,
    open: Mutex<bool>,
    open_cv: Condvar,
}

impl Gate {
    fn new() -> Self {
        Self {
            entered: Mutex::new(false),
            entered_cv: Condvar::new(),
            open: Mutex::new(false),
            open_cv: Condvar::new(),
        }
    }

    fn enter_and_wait(&self) {
        *self.entered.lock().unwrap() = true;
        self.entered_cv.notify_all();
        let mut open = self.open.lock().unwrap();
        while !*open {
            open = self.open_cv.wait(open).unwrap();
        }
    }

    fn wait_entered(&self) {
        let mut entered = self.entered.lock().unwrap();
        while !*entered {
            entered = self.entered_cv.wait(entered).unwrap();
        }
    }

    fn release(&self) {
        *self.open.lock().unwrap() = true;
        self.open_cv.notify_all();
    }
}

/// 断言失败/panic 时也要放行 gate：否则被阻塞的 blocking 工作永远不结束，
/// 测试会挂死而不是干净地失败。
struct ReleaseOnDrop(Arc<Gate>);

impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        self.0.release();
    }
}

/// 注入的浏览文件访问：`read_nfo` 阻塞在 gate 上，用来证明它不在 Store 锁内；
/// `metadata` 仍走真实磁盘，让候选 NFO 的 `is_file()` 判定照常工作。
struct GatedFiles {
    gate: Arc<Gate>,
}

impl BrowseFileRead for GatedFiles {
    fn read_nfo(&self, _path: &Path) -> Option<library::NfoMeta> {
        self.gate.enter_and_wait();
        Some(library::NfoMeta {
            rating: Some("9.1".into()),
            runtime_minutes: Some("123".into()),
            original_language: Some("en".into()),
            ..Default::default()
        })
    }

    fn metadata(&self, path: &Path) -> std::io::Result<std::fs::Metadata> {
        std::fs::metadata(path)
    }
}

fn app_with(state: ApiState) -> axum::Router {
    router(state)
}

fn base_state(tmp: &tempfile::TempDir) -> ApiState {
    state(
        tmp.path(),
        Arc::new(Fixtures {
            requests: ParkingMutex::new(Vec::new()),
            bodies: HashMap::new(),
        }),
        Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
    )
}

fn movie_library_id(state: &ApiState) -> String {
    state
        .store()
        .lock()
        .list_libraries()
        .unwrap()
        .into_iter()
        .find(|library| library.kind == MediaKind::Movie)
        .expect("default movie library")
        .id
}

fn library_roots(state: &ApiState, library_id: &str) -> Vec<PathBuf> {
    state
        .store()
        .lock()
        .list_libraries()
        .unwrap()
        .into_iter()
        .find(|library| library.id == library_id)
        .expect("library")
        .root_paths
}

/// 写一个真实视频文件 + 真实 NFO；返回 ledger 行使用的路径。
fn seed_row(state: &ApiState, media_id: MediaId, path: &Path, nfo: Option<&str>) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, b"video-bytes").unwrap();
    if let Some(body) = nfo {
        std::fs::write(path.with_extension("nfo"), body).unwrap();
    }
    let row = LedgerRow {
        id: LedgerId::new(),
        media_id,
        path: path.to_string_lossy().to_string(),
        season: None,
        episode: None,
        resolution: Some("1080p".into()),
        codec: Some("H264".into()),
        hdr: None,
        quality_source: QualitySource::Probe,
        confidence: Confidence::High,
        filter_score: None,
    };
    state.store().lock().insert_ledger(&row).unwrap();
}

fn insert_media(state: &ApiState, media_id: MediaId, title: &str) {
    let media = Media {
        id: media_id,
        kind: MediaKind::Movie,
        title: title.into(),
        year: Some(2024),
        original_title: Some(title.into()),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    state.store().lock().insert_media(&media).unwrap();
}

fn seed_movie(
    state: &ApiState,
    media_id: MediaId,
    title: &str,
    path: &Path,
    nfo: Option<&str>,
) {
    insert_media(state, media_id, title);
    seed_row(state, media_id, path, nfo);
}

async fn get_json(app: &axum::Router, url: &str) -> Value {
    let response = app
        .clone()
        .oneshot(request("GET", url, Some("management-secret"), Value::Null))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "{url}");
    json_data(response).await
}

/// 锁外证据：NFO 读取阻塞时，Store 锁必须仍然可用。
///
/// 基线在 `select()` 里持锁读 NFO，这条断言会失败。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nfo_read_happens_outside_the_store_lock() {
    let tmp = tempfile::tempdir().unwrap();
    let gate = Arc::new(Gate::new());
    let api_state = base_state(&tmp).with_browse_file_read(Arc::new(GatedFiles {
        gate: gate.clone(),
    }));
    let app = app_with(api_state.clone());
    let library_id = movie_library_id(&api_state);
    let root = library_roots(&api_state, &library_id).remove(0);
    let video = root.join("Locked").join("Locked.mkv");
    seed_movie(&api_state, MediaId::new(), "Locked", &video, Some("<movie/>"));

    let request_app = app.clone();
    let url = format!("/api/v1/libraries/{library_id}/items");
    let pending = tokio::spawn(async move { request_app.oneshot(request("GET", &url, Some("management-secret"), Value::Null)).await });

    gate.wait_entered();
    let _release_on_drop = ReleaseOnDrop(gate.clone());
    assert!(
        api_state.store().try_lock().is_some(),
        "NFO 读取期间不得持有 Store 锁"
    );
    gate.release();

    let response = pending.await.unwrap().unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

/// NFO 决定的过滤/排序仍按原语义生效（真实磁盘 NFO，走默认 LocalBrowseFileRead）。
#[tokio::test]
async fn nfo_metadata_still_drives_filter_and_sort() {
    let tmp = tempfile::tempdir().unwrap();
    let api_state = base_state(&tmp);
    let app = app_with(api_state.clone());
    let library_id = movie_library_id(&api_state);
    let root = library_roots(&api_state, &library_id).remove(0);

    let high = MediaId::new();
    seed_movie(
        &api_state,
        high,
        "High",
        &root.join("High").join("High.mkv"),
        Some("<movie><rating>8.5</rating><runtime>150</runtime><original_language>en</original_language></movie>"),
    );
    let low = MediaId::new();
    seed_movie(
        &api_state,
        low,
        "Low",
        &root.join("Low").join("Low.mkv"),
        Some("<movie><rating>5.5</rating><runtime>90</runtime><original_language>ja</original_language></movie>"),
    );

    let all = get_json(&app, &format!("/api/v1/libraries/{library_id}/items?sort=rating&order=desc")).await;
    let titles: Vec<&str> = all
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles, vec!["High", "Low"], "rating desc 排序");

    let filtered = get_json(
        &app,
        &format!("/api/v1/libraries/{library_id}/items?rating_gte=8"),
    )
    .await;
    let titles: Vec<&str> = filtered
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles, vec!["High"], "NFO rating 过滤");
    assert_eq!(filtered.as_array().unwrap()[0]["file_count"], 1);
}

/// 嵌套根 + 同一 Media 在两个库：各自只返回自己名下的文件。
#[tokio::test]
async fn nested_libraries_keep_their_own_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let api_state = base_state(&tmp);
    let app = app_with(api_state.clone());
    let parent_id = movie_library_id(&api_state);
    let parent_root = library_roots(&api_state, &parent_id).remove(0);

    let child_root = parent_root.join("child");
    std::fs::create_dir_all(&child_root).unwrap();
    let child = api_state
        .store()
        .lock()
        .create_library(
            MediaKind::Movie,
            "child",
            &[child_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();

    let shared = MediaId::new();
    insert_media(&api_state, shared, "Shared");
    seed_row(
        &api_state,
        shared,
        &parent_root.join("Shared").join("Shared.mkv"),
        None,
    );
    seed_row(
        &api_state,
        shared,
        &child_root.join("Shared").join("Shared.mkv"),
        None,
    );

    let parent_items = get_json(&app, &format!("/api/v1/libraries/{parent_id}/items")).await;
    let parent_rows = parent_items.as_array().unwrap();
    assert_eq!(parent_rows.len(), 1);
    assert_eq!(
        parent_rows[0]["file_count"], 1,
        "父库不得把子库文件算进自己的条目"
    );

    let child_items = get_json(&app, &format!("/api/v1/libraries/{}/items", child.id)).await;
    let child_rows = child_items.as_array().unwrap();
    assert_eq!(child_rows.len(), 1);
    assert_eq!(child_rows[0]["file_count"], 1, "子库只应看到自己的文件");
}

/// 空图组必须在 gallery 分页**之前**剔除。
#[tokio::test]
async fn gallery_drops_empty_artwork_groups_before_paging() {
    let tmp = tempfile::tempdir().unwrap();
    let api_state = base_state(&tmp);
    let app = app_with(api_state.clone());
    let library_id = movie_library_id(&api_state);
    let root = library_roots(&api_state, &library_id).remove(0);

    // 标题排序下 "A NoArt" 在前，但它没有任何图片。
    seed_movie(
        &api_state,
        MediaId::new(),
        "A NoArt",
        &root.join("A NoArt").join("A NoArt.mkv"),
        None,
    );
    let with_art = root.join("B WithArt");
    let with_art_video = with_art.join("B WithArt.mkv");
    seed_movie(&api_state, MediaId::new(), "B WithArt", &with_art_video, None);
    std::fs::write(with_art.join("poster.jpg"), b"poster").unwrap();

    let page = get_json(&app, &format!("/api/v1/libraries/{library_id}/gallery?limit=1")).await;
    let groups = page.as_array().unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(
        groups[0]["title"], "B WithArt",
        "空图组应先剔除，offset=0 才能拿到有图的组"
    );
    assert!(!groups[0]["images"].as_array().unwrap().is_empty());
}
