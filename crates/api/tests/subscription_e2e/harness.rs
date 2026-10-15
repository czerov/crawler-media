//! End-to-end harness: real `ApiState` + router + SQLite in a temp dir, a
//! fixture indexer (NexusPHP HTML), and a stateful mock qBittorrent.
//! Probing uses the real `ffprobe` on synthetic bytes, i.e. the probe-failure
//! branch, unless a scenario says otherwise.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use api::{ApiState, router};
use axum::http::StatusCode;
use domain::{MediaKind, SubscribeId};
use parking_lot::Mutex;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::common::*;
pub(crate) use super::common::{install_fake_ffprobe, probed_video, unprobeable_video};
use super::mock_qbit::MockQbit;

pub const TOKEN: &str = "management-secret";
pub const MATRIX_TITLE: &str = "The.Matrix.1999.2160p.BluRay.x265-GROUP";
pub const MATRIX_ENCLOSURE: &str = "https://pt.example/download.php?id=1&passkey=abc";

pub struct Harness {
    pub tmp: tempfile::TempDir,
    pub state: ApiState,
    pub app: axum::Router,
    pub qb: Arc<MockQbit>,
}

impl Harness {
    pub fn new() -> Self {
        Self::with_search_html(nexusphp())
    }

    pub fn with_search_html(html: &'static str) -> Self {
        install_fake_ffprobe();
        let tmp = tempfile::tempdir().unwrap();
        let qb = MockQbit::start(&tmp.path().join("downloads"));
        let client = downloader::QbitDownloader::connect(qb.config()).unwrap();
        let fixtures = Arc::new(Fixtures {
            requests: Mutex::new(vec![]),
            bodies: HashMap::from([("search", html)]),
        });
        let state = state(tmp.path(), fixtures, Arc::new(client));
        let app = router(state.clone());
        Self {
            tmp,
            state,
            app,
            qb,
        }
    }

    pub async fn create_site(&self) {
        create_site(&self.app).await;
    }

    /// Movie subscription for The Matrix (2160p filter); `patch` is merged in.
    pub async fn create_movie_subscribe(&self, patch: Value) -> SubscribeId {
        let mut payload = subscribe_payload("search");
        if let (Some(target), Some(extra)) = (payload.as_object_mut(), patch.as_object()) {
            for (k, v) in extra {
                target.insert(k.clone(), v.clone());
            }
        }
        let response = self
            .app
            .clone()
            .oneshot(request(
                "POST",
                "/api/v1/subscriptions",
                Some(TOKEN),
                payload,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        json_data(response).await["id"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap()
    }

    /// Manual "run now": search → admit → submit to downloader → record pending.
    pub async fn run_subscribe(&self, id: SubscribeId) -> Value {
        let response = self
            .app
            .clone()
            .oneshot(request(
                "POST",
                &format!("/api/v1/subscriptions/{id}/run"),
                Some(TOKEN),
                Value::Null,
            ))
            .await
            .unwrap();
        let status = response.status();
        let body = json_body(response).await;
        assert_eq!(status, StatusCode::OK, "run failed: {body}");
        body
    }

    /// Scheduled Transfer for one subscription (collect completed → import).
    pub async fn transfer(&self, id: SubscribeId) -> Result<(), String> {
        let state = self.state.clone();
        tokio::task::spawn_blocking(move || api::worker::transfer_one(&state, id))
            .await
            .unwrap()
    }

    /// Task-center listing (also performs the absent-pending cleanup).
    pub async fn list_tasks(&self) -> Value {
        let response = self
            .app
            .clone()
            .oneshot(request(
                "GET",
                "/api/v1/downloaders/tasks",
                Some(TOKEN),
                Value::Null,
            ))
            .await
            .unwrap();
        json_body(response).await
    }

    pub fn pending(&self, id: SubscribeId) -> Vec<(i32, api::store::PendingDownload)> {
        self.state.store().lock().load_pending(id).unwrap()
    }

    pub fn pending_state(&self, id: SubscribeId, state: &str) -> usize {
        self.state
            .store()
            .lock()
            .load_pending_state(id, state)
            .unwrap()
            .len()
    }

    /// Time control for wall-clock based grace periods: back-date every
    /// active pending row's `submitted_at` by `secs`.
    pub fn age_pending(&self, id: SubscribeId, secs: i64) {
        let store = self.state.store();
        let store = store.lock();
        for (score, mut pending) in store.load_pending(id).unwrap() {
            pending.submitted_at = Some(api::job_loop::unix_now() - secs);
            store
                .record_pending_submission(id, score, &pending)
                .unwrap();
        }
    }

    pub fn movie_library_root(&self) -> PathBuf {
        self.state
            .store()
            .lock()
            .default_library(MediaKind::Movie)
            .unwrap()
            .unwrap()
            .root_paths[0]
            .clone()
    }

    pub fn ledger_paths(&self) -> Vec<String> {
        self.state
            .store()
            .lock()
            .list_ledger()
            .unwrap()
            .into_iter()
            .map(|r| r.path)
            .collect()
    }

    pub fn library_files(&self) -> Vec<PathBuf> {
        fn walk(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, out);
                } else if crate::is_video(&path) {
                    out.push(path);
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.movie_library_root(), &mut out);
        out.sort();
        out
    }
}

pub fn wash_payload() -> Value {
    json!({ "wash_cut": true })
}

