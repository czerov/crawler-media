//! End-to-end harness: real `ApiState` + router + SQLite in a temp dir, a
//! fixture indexer (NexusPHP HTML), and a stateful mock qBittorrent.
//! Probing uses the real `ffprobe` on synthetic bytes, i.e. the probe-failure
//! branch, unless a scenario says otherwise.

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

/// Indexer fixture whose search page can change between rounds.
pub struct SearchFixture {
    pub html: Mutex<String>,
    pub requests: Mutex<Vec<String>>,
}

impl SearchFixture {
    pub fn empty() -> Self {
        Self {
            html: Mutex::new(String::new()),
            requests: Mutex::new(vec![]),
        }
    }
}

impl indexer::Fetcher for SearchFixture {
    fn fetch(&self, request: &indexer::FetchRequest) -> Result<String, indexer::IndexerError> {
        self.requests.lock().push(request.key.clone());
        if request.key.starts_with("search:") {
            Ok(self.html.lock().clone())
        } else {
            Err(indexer::IndexerError::Fetch(format!(
                "no fixture for {}",
                request.key
            )))
        }
    }
}

/// NexusPHP search page with one row per `(title, id)`; enclosure is
/// `https://pt.example/download.php?id=<id>&passkey=abc`.
pub fn nexus_rows(rows: &[(&str, u32)]) -> String {
    let mut html = String::from("<!DOCTYPE html><html><body><table class=\"torrents\">");
    for (title, id) in rows {
        html.push_str(&format!(
            r#"<tr class="torrent"><td><a class="name" title="{title}" href="details.php?id={id}">x</a></td><td><a class="download" href="download.php?id={id}&amp;passkey=abc"></a></td><td class="size">4.00 GB</td><td class="seeders">42</td><td class="leechers">7</td><td class="snatched">180</td><td class="upload_time">2024-01-15 08:30</td><td class="category">Movies</td></tr>"#
        ));
    }
    html.push_str("</table></body></html>");
    html
}

pub fn enclosure(id: u32) -> String {
    format!("https://pt.example/download.php?id={id}&passkey=abc")
}

pub struct Harness {
    pub search: Arc<SearchFixture>,
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
        Self::build(html, None)
    }

    /// Harness whose Catalog (TMDB stand-in) is `catalog`.
    pub fn with_catalog(catalog: Arc<dyn api::catalog::Catalog>) -> Self {
        Self::build(nexusphp(), Some(catalog))
    }

    /// Harness whose default downloader is `client` (e.g. a Transmission
    /// pointed at `MockTransmission`); the qB mock still runs but is unused.
    pub fn with_downloader(client: Arc<dyn downloader::Downloader>) -> Self {
        Self::build_with(nexusphp(), None, Some(client))
    }

    fn build(html: &str, catalog: Option<Arc<dyn api::catalog::Catalog>>) -> Self {
        Self::build_with(html, catalog, None)
    }

    /// Simulate a process restart: a fresh ApiState (and downloader client) on
    /// the same data directory.
    pub fn restart(&mut self, client: Arc<dyn downloader::Downloader>) {
        let state = ApiState::new(
            api::Store::open(self.tmp.path().join("data")).unwrap(),
            TOKEN.into(),
            indexer::ProfileSet::load(None).unwrap(),
            self.search.clone(),
            client,
            self.tmp.path().join("library"),
        )
        .unwrap();
        self.app = router(state.clone());
        self.state = state;
    }

    fn build_with(
        html: &str,
        catalog: Option<Arc<dyn api::catalog::Catalog>>,
        client: Option<Arc<dyn downloader::Downloader>>,
    ) -> Self {
        install_fake_ffprobe();
        let tmp = tempfile::tempdir().unwrap();
        let qb = MockQbit::start(&tmp.path().join("downloads"));
        let client: Arc<dyn downloader::Downloader> = match client {
            Some(client) => client,
            None => Arc::new(downloader::QbitDownloader::connect(qb.config()).unwrap()),
        };
        let search = Arc::new(SearchFixture {
            html: Mutex::new(html.to_string()),
            requests: Mutex::new(vec![]),
        });
        let state = ApiState::new(
            api::Store::open(tmp.path().join("data")).unwrap(),
            TOKEN.into(),
            indexer::ProfileSet::load(None).unwrap(),
            search.clone(),
            client,
            tmp.path().join("library"),
        )
        .unwrap();
        let state = match catalog {
            Some(catalog) => state.with_catalog(catalog),
            None => state,
        };
        let app = router(state.clone());
        Self {
            search,
            tmp,
            state,
            app,
            qb,
        }
    }

    /// Number of keyword searches the indexer fixture has served.
    pub fn search_requests(&self) -> usize {
        self.search
            .requests
            .lock()
            .iter()
            .filter(|k| k.starts_with("search:"))
            .count()
    }

    pub fn set_search_rows(&self, rows: &[(&str, u32)]) {
        *self.search.html.lock() = nexus_rows(rows);
    }

    /// Like `set_search_rows`, with a per-row size cell such as `"300 MB"`.
    pub fn set_search_rows_sized(&self, rows: &[(&str, u32, &str)]) {
        let plain: Vec<(&str, u32)> = rows.iter().map(|(t, id, _)| (*t, *id)).collect();
        let mut html = nexus_rows(&plain);
        for (title, _, size) in rows {
            let marker = format!("title=\"{title}\"");
            let at = html.find(&marker).expect("row");
            let cell = html[at..].find("4.00 GB").expect("size cell") + at;
            html.replace_range(cell..cell + "4.00 GB".len(), size);
        }
        *self.search.html.lock() = html;
    }

    /// Seed an owned movie file (library + ledger + facts) of the given quality.
    pub fn seed_owned_movie(
        &self,
        id: SubscribeId,
        height: u32,
        codec: &str,
        score: i32,
    ) -> PathBuf {
        let media_id = self
            .state
            .store()
            .lock()
            .get_subscribe(id)
            .unwrap()
            .unwrap()
            .media_id;
        let owned = self
            .movie_library_root()
            .join("The Matrix (1999)")
            .join(format!("The Matrix (1999) - {height}p.mkv"));
        std::fs::create_dir_all(owned.parent().unwrap()).unwrap();
        std::fs::write(&owned, probed_video(height, codec, false)).unwrap();
        let row = domain::LedgerRow {
            id: domain::LedgerId::new(),
            media_id,
            path: owned.display().to_string(),
            season: None,
            episode: None,
            resolution: Some(format!("{height}p")),
            codec: Some(codec.into()),
            hdr: None,
            quality_source: domain::QualitySource::Probe,
            confidence: domain::Confidence::High,
            filter_score: Some(score),
        };
        let mut facts = subscribe::SubscribeFacts::default();
        facts.replace(
            None,
            None,
            subscribe::QualityFact {
                score,
                path: Some(row.path.clone()),
            },
        );
        let codec_tag = if codec == "hevc" { "x265" } else { "x264" };
        facts.set_quality(
            row.path.clone(),
            release::parse(&format!("The.Matrix.1999.{height}p.WEB-DL.{codec_tag}")),
        );
        let store = self.state.store();
        let store = store.lock();
        store.insert_ledger(&row).unwrap();
        store.save_subscribe_facts(id, &facts).unwrap();
        owned
    }

    pub fn owned_movie_path(&self, id: SubscribeId) -> Option<String> {
        self.state
            .store()
            .lock()
            .load_subscribe_facts(id)
            .unwrap()
            .movie()
            .and_then(|f| f.path.clone())
    }

    /// Site whose download links point at the mock (serves `torrent_file`).
    pub async fn create_site_at_mock(&self) {
        let mut payload = site_payload();
        payload["url"] = json!(format!("{}/", self.qb.url));
        let response = self
            .app
            .clone()
            .oneshot(request("POST", "/api/v1/sites", Some(TOKEN), payload))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
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

    /// Manual run returning status and body without asserting success.
    pub async fn run_subscribe_raw(&self, id: SubscribeId) -> (StatusCode, Value) {
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
        (status, json_body(response).await)
    }

    /// Scheduled SubscribeSearch job (finish_search path) run in-process.
    pub async fn scheduled_search(&self, id: SubscribeId) -> Result<(), String> {
        let state = self.state.clone();
        tokio::task::spawn_blocking(move || {
            use jobs::Runner;
            let payload = json!({ "subscribe_id": id.to_string() }).to_string();
            let job = state
                .jobs()
                .lock()
                .enqueue(jobs::JobKind::SubscribeSearch, &payload, 0)
                .unwrap();
            api::worker::StateRunner {
                state: state.clone(),
            }
            .run(&job)
        })
        .await
        .unwrap()
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
        let tv = self.state.store().lock().default_library(MediaKind::Tv);
        if let Ok(Some(tv)) = tv {
            let tv = PathBuf::from(tv.root_paths[0].clone());
            if tv != self.movie_library_root() {
                walk(&tv, &mut out);
            }
        }
        out.sort();
        out
    }
}

pub fn wash_payload() -> Value {
    json!({ "wash_cut": true })
}
