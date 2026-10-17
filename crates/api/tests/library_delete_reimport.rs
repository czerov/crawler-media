//! Public HTTP removal/reimport contract. No runtime exists during ApiState construction:
//! probe workers are deliberately not started, and STRM targets must never be fetched.
use api::{ApiState, catalog::Catalog};
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use domain::{Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource, UserId};
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use store::{FingerprintCacheEntry, ProbeStage, ProbeStageKey};
use tower::ServiceExt;

#[derive(Default)]
struct OfflineCatalog(Mutex<Vec<String>>);
impl OfflineCatalog {
    fn reject<T>(&self, request: String) -> Result<T, String> {
        self.0.lock().push(request);
        Err("network forbidden in reimport fixture".into())
    }
}
impl Catalog for OfflineCatalog {
    fn search_movie(&self, query: &str) -> Result<Vec<media::CatalogHit>, String> {
        self.reject(format!("search:{query}"))
    }
    fn search_tv(&self, query: &str) -> Result<Vec<media::CatalogHit>, String> {
        self.search_movie(query)
    }
    fn popular_movie(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(vec![])
    }
    fn popular_tv(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(vec![])
    }
    fn details(&self, _: MediaKind, id: &str) -> Result<Option<Media>, String> {
        self.reject(format!("details:{id}"))
    }
    fn metadata_with_preferences(
        &self,
        _: MediaKind,
        id: &str,
        _: &[String],
        _: &[String],
    ) -> Result<Option<media::ItemMeta>, String> {
        self.reject(format!("metadata:{id}"))
    }
    fn poster_url(&self, _: MediaKind, id: &str) -> Result<Option<String>, String> {
        self.reject(format!("poster:{id}"))
    }
}
struct OnlineCatalog {
    hit: media::CatalogHit,
    requests: Mutex<Vec<String>>,
}
impl Catalog for OnlineCatalog {
    fn search_movie(&self, query: &str) -> Result<Vec<media::CatalogHit>, String> {
        self.requests.lock().push(query.to_string());
        Ok(vec![self.hit.clone()])
    }
    fn search_tv(&self, _: &str) -> Result<Vec<media::CatalogHit>, String> { Ok(vec![]) }
    fn popular_movie(&self) -> Result<Vec<media::CatalogHit>, String> { Ok(vec![]) }
    fn popular_tv(&self) -> Result<Vec<media::CatalogHit>, String> { Ok(vec![]) }
    fn details(&self, _: MediaKind, _: &str) -> Result<Option<Media>, String> { Ok(None) }
}

struct NoFetch;
impl Fetcher for NoFetch {
    fn fetch(&self, _: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("network forbidden".into()))
    }
}

struct Fixture {
    _temp: tempfile::TempDir,
    state: ApiState,
    catalog: Arc<OfflineCatalog>,
    root: PathBuf,
    library_id: String,
    old: LedgerRow,
    other: LedgerRow,
    other_library_id: String,
    stage: ProbeStageKey,
    users: [UserId; 2],
    bytes: Vec<(PathBuf, Vec<u8>)>,
    kind: MediaKind,
}

fn row(media_id: MediaId, path: &Path, kind: MediaKind) -> LedgerRow {
    LedgerRow {
        id: LedgerId::new(),
        media_id,
        path: path.display().to_string(),
        season: (kind == MediaKind::Tv).then_some(1),
        episode: (kind == MediaKind::Tv).then_some(3),
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
    }
}
fn cache() -> FingerprintCacheEntry {
    FingerprintCacheEntry {
        cache_key: "old-source-v1".into(),
        algorithm_version: 1,
        sample_duration_secs: 180,
        media_duration_ms: Some(1_000_000),
        intro: vec![1, 3, 5],
        outro: Some(vec![2, 4, 6]),
    }
}
impl Fixture {
    fn new(kind: MediaKind, incoming_alias: &str) -> Self {
        assert!(tokio::runtime::Handle::try_current().is_err());
        let temp = tempfile::tempdir().unwrap();
        let catalog = Arc::new(OfflineCatalog::default());
        let state = ApiState::new(
            store::Store::open(temp.path().join("data")).unwrap(),
            "secret".into(),
            ProfileSet::load(None).unwrap(),
            Arc::new(NoFetch),
            Arc::new(downloader::MemoryDownloader::new(temp.path().join("stage"))),
            temp.path().join("library"),
        )
        .unwrap()
        .with_catalog(catalog.clone());
        let root = temp.path().join("reimport");
        let other_root = temp.path().join("independent");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&other_root).unwrap();
        let (filename, bytes) = sidecars(&root, kind, incoming_alias);
        let shared = state.store();
        let store = shared.lock();
        let (library_id, other_library_id) = create_libraries(&store, kind, &root, &other_root);
        let media = canonical_media(kind);
        store.insert_media(&media).unwrap();
        let old = row(media.id, &root.join(filename), kind);
        let other = row(media.id, &other_root.join(filename), kind);
        std::fs::write(&other.path, b"independent file").unwrap();
        seed_caches(&store, [&old, &other]);
        let stage = ProbeStageKey {
            ledger_id: old.id.to_string(),
            context_key: "old-context".into(),
            stage: ProbeStage::Outro,
        };
        assert!(store.claim_probe_stage(&stage, "old-probe", 100).unwrap());
        let users = [UserId::new(), UserId::new()];
        seed_progress(&store, users, &media);
        drop(store);
        Self {
            _temp: temp,
            state,
            catalog,
            root,
            library_id,
            other_library_id,
            old,
            other,
            stage,
            users,
            bytes,
            kind,
        }
    }
}

fn sidecars(
    root: &Path,
    kind: MediaKind,
    incoming_alias: &str,
) -> (&'static str, Vec<(PathBuf, Vec<u8>)>) {
    let (filename, nfo_name, xml_root) = if kind == MediaKind::Tv {
        ("English.Name.2025.S01E03.strm", "tvshow.nfo", "tvshow")
    } else {
        ("English.Name.2025.strm", "English.Name.2025.nfo", "movie")
    };
    let display_title = if incoming_alias == "207890" {
        "本地中文名称"
    } else {
        "旧中文名称"
    };
    let nfo = format!(
        "<{xml_root}><title>{display_title}</title><year>2022</year><uniqueid type=\"tmdb\">{incoming_alias}</uniqueid></{xml_root}>"
    );
    let mut bytes = vec![
        (
            root.join(filename),
            b"https://invalid.example/never-fetch\n".to_vec(),
        ),
        (root.join(nfo_name), nfo.into_bytes()),
        (
            root.join("poster.jpg"),
            b"unchanged poster fixture".to_vec(),
        ),
        (
            root.join("fanart.jpg"),
            b"unchanged fanart fixture".to_vec(),
        ),
        (
            root.join("English.Name.srt"),
            b"unchanged subtitle fixture".to_vec(),
        ),
    ];
    if kind == MediaKind::Tv {
        bytes.push((root.join("English.Name.2025.S01E03.nfo"),
                b"<episodedetails><title>Episode</title><season>1</season><episode>3</episode><uniqueid type=\"tmdb\">999999</uniqueid></episodedetails>".to_vec()));
    }
    for (path, body) in &bytes {
        std::fs::write(path, body).unwrap();
    }
    (filename, bytes)
}

fn create_libraries(
    store: &store::Store,
    kind: MediaKind,
    root: &Path,
    other_root: &Path,
) -> (String, String) {
    let library = store
        .create_library(
            kind,
            "Remove me",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let other_library = store
        .create_library(
            kind,
            "Keep me",
            &[other_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    (library.id, other_library.id)
}

fn canonical_media(kind: MediaKind) -> Media {
    Media {
        id: MediaId::new(),
        kind,
        title: "旧中文名称".into(),
        year: Some(2022),
        original_title: None, aliases: Vec::new(),
        tmdb_id: Some("207890".into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    }
}

fn seed_caches(store: &store::Store, entries: [&LedgerRow; 2]) {
    for entry in entries {
        store.insert_ledger(entry).unwrap();
        store
            .put_file_meta(
                &entry.id.to_string(),
                &library::Tracks {
                    video: Some(library::VideoTrack {
                        codec: Some("h264".into()),
                        width: Some(1920),
                        height: Some(1080),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )
            .unwrap();
        store
            .put_fingerprint_cache(&entry.id.to_string(), &cache())
            .unwrap();
    }
}

fn seed_progress(store: &store::Store, users: [UserId; 2], media: &Media) {
    let (season, episode) = if media.kind == MediaKind::Tv {
        (1, 3)
    } else {
        (0, 0)
    };
    for (user, position, favorite, watched) in [
        (users[0], 750_000, true, false),
        (users[1], 120_000, false, true),
    ] {
        store
            .upsert_unit(
                user,
                media.id,
                season,
                episode,
                position,
                Some(watched),
                Some(favorite),
                Some(1_000_000),
                None,
                None,
                false,
                1,
            )
            .unwrap();
    }
}

impl Fixture {
    fn assert_preserved(&self) {
        for (path, body) in &self.bytes {
            assert_eq!(
                &std::fs::read(path).unwrap(),
                body,
                "{} changed",
                path.display()
            );
        }
        let shared = self.state.store();
        let store = shared.lock();
        assert!(store.get_library(&self.other_library_id).unwrap().is_some());
        assert_eq!(
            store.ledger_by_path(&self.other.path).unwrap().unwrap().id,
            self.other.id
        );
        assert!(
            store
                .get_file_meta(&self.other.id.to_string())
                .unwrap()
                .is_some()
        );
        assert_eq!(
            store
                .get_fingerprint_cache(&self.other.id.to_string())
                .unwrap()
                .unwrap(),
            cache()
        );
        assert_eq!(
            std::fs::read(&self.other.path).unwrap(),
            b"independent file"
        );
        let (season, episode) = if self.kind == MediaKind::Tv {
            (1, 3)
        } else {
            (0, 0)
        };
        for (user, position, favorite, watched) in [
            (self.users[0], 750_000, true, false),
            (self.users[1], 120_000, false, true),
        ] {
            let progress = store
                .unit_state(user, self.old.media_id, season, episode)
                .unwrap()
                .unwrap();
            assert_eq!(progress.position_ms, position);
            assert_eq!(progress.favorite, favorite);
            assert_eq!(progress.played, watched);
        }
    }
}

async fn call(app: &Router, method: &str, path: &str, payload: Value, status: StatusCode) -> Value {
    let request = Request::builder()
        .method(method)
        .uri(path)
        .header("Authorization", "Bearer secret")
        .header("Content-Type", "application/json")
        .body(Body::from(payload.to_string()))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let actual = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(actual, status, "{method} {path}: {body}");
    assert_eq!(body["ok"], true, "{body}");
    body["data"].clone()
}
async fn remove(f: &Fixture, app: &Router) {
    let operation = call(
        app,
        "DELETE",
        &format!("/api/v1/libraries/{}", f.library_id),
        Value::Null,
        StatusCode::ACCEPTED,
    )
    .await;
    let id = operation["operation_id"].as_str().unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let status = call(
                app,
                "GET",
                &format!("/api/v1/library-removals/{id}"),
                Value::Null,
                StatusCode::OK,
            )
            .await;
            assert_ne!(status["state"], "failed", "{status}");
            if status["state"] == "succeeded" {
                assert_eq!(status["removed_ledger_count"], 1);
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("removal did not reach a terminal state");
    let shared = f.state.store();
    let store = shared.lock();
    assert!(store.get_library(&f.library_id).unwrap().is_none());
    assert!(store.ledger_by_path(&f.old.path).unwrap().is_none());
    assert!(store.get_media(f.old.media_id).unwrap().is_some());
    assert!(
        store
            .get_file_meta(&f.old.id.to_string())
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .get_fingerprint_cache(&f.old.id.to_string())
            .unwrap()
            .is_none()
    );
    assert!(store.get_probe_stage(&f.stage).unwrap().is_none());
    drop(store);
    f.assert_preserved();
}
async fn scan_library_op(app: &Router, library_id: &str) {
    let operation = call(
        app,
        "POST",
        &format!("/api/v1/libraries/{library_id}/scan"),
        Value::Null,
        StatusCode::ACCEPTED,
    )
    .await;
    let op_id = operation["operation_id"].as_str().unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let status = call(
                app,
                "GET",
                &format!("/api/v1/libraries/{library_id}/scan-operations/{op_id}"),
                Value::Null,
                StatusCode::OK,
            )
            .await;
            if status["status"] == "completed" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
}

async fn reimport(f: &Fixture, app: &Router) -> LedgerRow {
    let created = call(
        app,
        "POST",
        "/api/v1/libraries",
        json!({
            "kind": if f.kind == MediaKind::Tv { "tv" } else { "movie" },
            "name": "Recreated", "root_paths": [f.root],
        }),
        StatusCode::OK,
    )
    .await;
    let id = created["id"].as_str().unwrap();
    assert_ne!(id, f.library_id);
    scan_library_op(app, id).await;
    let shared = f.state.store();
    let store = shared.lock();
    let library = store.get_library(id).unwrap().unwrap();
    assert!(!library.detect_intros);
    assert!(!library.enable_fingerprint);
    let new = store.ledger_by_path(&f.old.path).unwrap().unwrap();
    assert_ne!(new.id, f.old.id);
    // No workers are started: a cache miss proves old file_meta was not copied.
    // Actual fake-probe completion is a separate acceptance case, not covered here.
    assert!(store.get_file_meta(&new.id.to_string()).unwrap().is_none());
    assert!(
        store
            .get_fingerprint_cache(&new.id.to_string())
            .unwrap()
            .is_none()
    );
    new
}
fn run_reimport(kind: MediaKind, incoming_alias: &str, same_identity: bool) {
    let fixture = Fixture::new(kind, incoming_alias);
    let app = api::router(fixture.state.clone());
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            remove(&fixture, &app).await;
            let new = reimport(&fixture, &app).await;
            assert_eq!(new.media_id == fixture.old.media_id, same_identity);
            assert_eq!(
                (new.season, new.episode),
                (fixture.old.season, fixture.old.episode)
            );
            if !same_identity {
                let shared = fixture.state.store();
                let store = shared.lock();
                for user in fixture.users {
                    assert!(
                        store
                            .unit_state(user, new.media_id, 1, 3)
                            .unwrap()
                            .is_none()
                    );
                }
            }
            fixture.assert_preserved();
            let requests = fixture.catalog.0.lock();
            assert!(
                requests.is_empty(),
                "complete local NFO unexpectedly requested {requests:?}"
            );
        });
}

#[test]
fn tv_delete_reimport_preserves_two_users_and_independent_same_media() {
    run_reimport(MediaKind::Tv, "207890", true);
}
#[test]
fn movie_delete_reimport_preserves_whole_movie_progress() {
    run_reimport(MediaKind::Movie, "207890", true);
}

#[test]
fn online_identity_rebind_succeeds_when_reimported_library_reuses_removed_root() {
    let mut fixture = Fixture::new(MediaKind::Movie, "207890");
    let app = api::router(fixture.state.clone());
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            remove(&fixture, &app).await;
            // 移除本地完备的 NFO，迫使重新扫描时无法直接读取本地身份，必须走到在线识别与重绑
            let local_nfo = fixture.root.join("English.Name.2025.nfo");
            if local_nfo.exists() {
                std::fs::remove_file(&local_nfo).unwrap();
            }
            let hit = media::CatalogHit {
                media: Media {
                    id: MediaId::new(), kind: MediaKind::Movie, title: "English Name".into(),
                    year: Some(2025), original_title: None, aliases: Vec::new(), tmdb_id: Some("207890".into()),
                    douban_id: None, tvdb_id: None, bangumi_id: None, anilist_id: None,
                },
                poster_path: None, backdrop_path: None, rating: None, overview: None,
            };
            let online_catalog = Arc::new(OnlineCatalog {
                hit,
                requests: Mutex::new(Vec::new()),
            });
            fixture.state = fixture.state.clone().with_catalog(online_catalog.clone());
            let app = api::router(fixture.state.clone());
            let created = call(
                &app,
                "POST",
                "/api/v1/libraries",
                json!({"kind":"movie", "name":"Recreated", "root_paths":[fixture.root]}),
                StatusCode::OK,
            ).await;
            let library_id = created["id"].as_str().unwrap();
            scan_library_op(&app, library_id).await;
            // 建库会自动调度首次扫描（见 library_config::create_library），它与这次
            // 显式扫描并发：先返回的那次看到 ledger 行已存在（inserted=0），不会重绑，
            // 而真正做重绑的那次可能还没跑完 —— 之前就是在这里读到临时 MediaId 而假失败。
            // 等该库的后台工作全部结束再断言；断言本身不放宽。
            tokio::time::timeout(
                Duration::from_secs(30),
                fixture.state.wait_library_idle(library_id),
            )
            .await
            .expect("library reimport work did not settle");
            assert!(!online_catalog.requests.lock().is_empty(), "必须真正走到在线搜索");
            let shared = fixture.state.store();
            let store = shared.lock();
            let row = store.ledger_by_path(&fixture.old.path).unwrap().unwrap();
            assert_eq!(row.media_id, fixture.old.media_id);
            for user in fixture.users {
                assert_eq!(store.unit_state(user, row.media_id, 0, 0).unwrap().unwrap().position_ms,
                    if user == fixture.users[0] { 750_000 } else { 120_000 });
            }
        });
}
#[test]
fn different_nfo_alias_does_not_restore_old_progress() {
    run_reimport(MediaKind::Tv, "207891", false);
}
