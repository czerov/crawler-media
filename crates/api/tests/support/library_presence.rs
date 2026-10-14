use std::path::{Path, PathBuf};
use std::sync::Arc;

use api::library_presence::LibraryFileRead;
use api::management::{ApiState, router};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use domain::{Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource, UserId, UserRole};
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use parking_lot::Mutex;
use serde_json::Value;
use tower::ServiceExt;

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline".into()))
    }
}

struct MemoryDownloader(PathBuf);
impl downloader::Downloader for MemoryDownloader {
    fn add(&self, _: &domain::Torrent) -> Result<(), downloader::DownloaderError> {
        Ok(())
    }
    fn completed_files(&self, _: &domain::Torrent) -> Result<Vec<PathBuf>, downloader::DownloaderError> {
        Ok(vec![])
    }
}

#[derive(Default)]
pub struct MockLibraryFiles {
    pub unreadable_roots: Mutex<Vec<PathBuf>>,
    pub files: Mutex<std::collections::HashMap<PathBuf, Result<bool, std::io::ErrorKind>>>,
}

impl LibraryFileRead for MockLibraryFiles {
    fn readable_root(&self, path: &Path) -> std::io::Result<()> {
        let unreadable = self.unreadable_roots.lock();
        if unreadable.iter().any(|u| u == path) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "mock root unreadable",
            ));
        }
        Ok(())
    }

    fn presence(&self, path: &Path) -> std::io::Result<bool> {
        let files = self.files.lock();
        if let Some(res) = files.get(path) {
            match res {
                Ok(val) => Ok(*val),
                Err(kind) => Err(std::io::Error::new(*kind, "mock presence error")),
            }
        } else {
            Ok(path.exists())
        }
    }
}

pub struct PresenceFixture {
    pub dir: tempfile::TempDir,
    pub state: ApiState,
    pub mock_files: Arc<MockLibraryFiles>,
    pub admin_token: String,
    pub member_token: String,
    pub library_id: String,
    pub root: PathBuf,
}

impl PresenceFixture {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = store::Store::open(dir.path().join("data")).unwrap();
        let admin_token = "admin-secret-token".to_string();

        let state = ApiState::new(
            store,
            admin_token.clone(),
            ProfileSet::load(None).unwrap(),
            Arc::new(NoFetch),
            Arc::new(MemoryDownloader(dir.path().join("stage"))),
            dir.path().join("library"),
        )
        .unwrap();

        let mock_files = Arc::new(MockLibraryFiles::default());
        let state = state.with_library_file_read(mock_files.clone());

        // Create member user and token
        let member_id = UserId::new();
        let member_token = "member-secret-token".to_string();
        {
            let store_arc = state.store();
            let guard = store_arc.lock();
            let user = domain::User {
                id: member_id,
                login: "member".into(),
                enabled: true,
                role: UserRole::Member,
            };
            guard.insert_user(&user).unwrap();
            guard.set_user_token(member_id, &member_token).unwrap();
        }

        // Create test library
        let root = dir.path().join("movies");
        std::fs::create_dir_all(&root).unwrap();
        let lib = {
            let store_arc = state.store();
            let guard = store_arc.lock();
            let root_str = root.to_str().unwrap();
            guard
                .create_library(
                    MediaKind::Movie,
                    "Movies",
                    &[root_str],
                    "everyone",
                    true,
                    &[],
                )
                .unwrap()
        };

        Self {
            dir,
            state,
            mock_files,
            admin_token,
            member_token,
            library_id: lib.id,
            root,
        }
    }

    /// 当前 Library 的扫描 scope 指纹（与生产 `ScanCoordinator::enqueue` 同一构造）。
    pub fn scope_signature(&self) -> String {
        let store = self.state.store();
        let guard = store.lock();
        let library = guard
            .get_library(&self.library_id)
            .unwrap()
            .expect("library must exist");
        api::scan_operations::scan_scope_signature(&library)
    }

    /// 轮询 store 直到 operation 进入终态，返回落库的 operation（含 report）。
    pub async fn wait_scan_terminal_op(&self, op_id: &str) -> store::ScanOperation {
        for _ in 0..600 {
            let op = self.state.store().lock().scan_operation(op_id).unwrap();
            if let Some(op) = op
                && !op.status.is_active()
            {
                return op;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        panic!("scan operation {op_id} 未在预期时间内进入终态");
    }

    pub fn insert_media_and_ledger(&self, rel_path: &str) -> (MediaId, LedgerRow) {
        let media_id = MediaId::new();
        let media = Media {
            id: media_id,
            kind: MediaKind::Movie,
            title: "Test Movie".to_string(),
            original_title: None,
            year: None,
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        };
        let file_path = self.root.join(rel_path);
        let row = LedgerRow {
            id: LedgerId::new(),
            media_id,
            path: file_path.to_str().unwrap().to_string(),
            season: None,
            episode: None,
            resolution: None,
            codec: None,
            hdr: None,
            quality_source: QualitySource::Release,
            confidence: Confidence::High,
            filter_score: None,
        };
        let store_arc = self.state.store();
        let guard = store_arc.lock();
        guard.insert_media(&media).unwrap();
        guard.insert_ledger(&row).unwrap();
        (media_id, row)
    }

    pub async fn request(
        &self,
        method: &str,
        uri: &str,
        token: Option<&str>,
    ) -> (StatusCode, Value) {
        self.request_with_body(method, uri, token, Value::Null).await
    }

    pub async fn request_with_body(
        &self,
        method: &str,
        uri: &str,
        token: Option<&str>,
        body: Value,
    ) -> (StatusCode, Value) {
        let app = router(self.state.clone());
        let mut builder = Request::builder().method(method).uri(uri);
        if let Some(t) = token {
            builder = builder.header("Authorization", format!("Bearer {}", t));
        }
        let req_body = if body.is_null() {
            Body::empty()
        } else {
            builder = builder.header("Content-Type", "application/json");
            Body::from(serde_json::to_vec(&body).unwrap())
        };
        let req = builder.body(req_body).unwrap();
        let resp = app.oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, json)
    }

    /// 等待扫描终态。`completed` 与 `partial` 都返回 200（旧调用方需要接受
    /// "部分根不可达" 的既有合同）；需要断言"真的全部完成"的调用方请用
    /// [`Self::scan_and_wait_completed`]，否则失败/部分完成会被掩盖。
    pub async fn scan_and_wait(&self, library_id: &str) -> (StatusCode, Value) {
        let (status, res) = self
            .request(
                "POST",
                &format!("/api/v1/libraries/{}/scan", library_id),
                Some(&self.admin_token),
            )
            .await;
        if status != StatusCode::ACCEPTED {
            return (status, res);
        }
        let op_id = res["data"]["operation_id"].as_str().expect("must return operation_id");
        for _ in 0..100 {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let (poll_status, poll_res) = self
                .request(
                    "GET",
                    &format!("/api/v1/libraries/{}/scan-operations/{}", library_id, op_id),
                    Some(&self.admin_token),
                )
                .await;
            if poll_status == StatusCode::OK {
                let status_str = poll_res["data"]["status"].as_str().unwrap_or("");
                match status_str {
                    "completed" | "partial" => return (StatusCode::OK, poll_res["data"]["report"].clone()),
                    "cancelled" => return (StatusCode::CONFLICT, poll_res["data"]["report"].clone()),
                    "failed" => return (StatusCode::INTERNAL_SERVER_ERROR, poll_res["data"]["report"].clone()),
                    _ => {}
                }
            }
        }
        panic!("scan operation timed out");
    }

    /// 严格等待：只有 `completed` 才算成功；`partial`/`failed`/`cancelled` 都会
    /// panic。断言"后续扫描真的恢复正常"必须用这个，不能用宽容版本。
    pub async fn scan_and_wait_completed(&self, library_id: &str) -> Value {
        let (status, res) = self.scan_and_wait(library_id).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "扫描未成功结束（HTTP {status}）: {res}"
        );
        assert_eq!(
            res["status"], "completed",
            "扫描必须真正完成，而不是 partial/failed: {res}"
        );
        res
    }
}
