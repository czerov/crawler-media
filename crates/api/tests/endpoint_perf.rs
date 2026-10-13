//! C5b: release-profile endpoint latency matrix for the read-path remediation.
//!
//! Manual only — run with:
//!
//! ```text
//! cargo test --release -p api --test endpoint_perf -- --ignored --nocapture --test-threads=1
//! ```
//!
//! `PERF_SIZES` / `PERF_MODES` / `PERF_SAMPLES` can narrow the matrix while
//! iterating; the published table in
//! `docs/audits/2026-10-12-remediation-review-repro/endpoint-perf.md` uses the
//! defaults. No network, no Chromium, no ffmpeg.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use api::{ApiState, Store, router};
use axum::body::{Body, to_bytes};
use axum::http::Request;
use domain::{Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource, UserId};
use store::SessionRow;
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use tower::ServiceExt;

const DEFAULT_SIZES: &[usize] = &[100, 500, 2_000, 10_000];
const DEFAULT_MODES: &[&str] = &["movie", "tv"];
const DEFAULT_SAMPLES: usize = 5;
const SEED: u64 = 0xC5B_2026;
const TOKEN: &str = "management-secret";

const ENDPOINTS: &[&str] = &[
    "items", "gallery", "facets", "search", "history", "stats", "jobs",
];

struct NoFetch;

impl Fetcher for NoFetch {
    fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline fixture".into()))
    }
}

/// Deterministic LCG so baseline and after share the same data *shape*.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn admin_user(store: &Store) -> UserId {
    store
        .list_users()
        .unwrap()
        .into_iter()
        .find(|user| store.user_role(user.id).ok().as_deref() == Some("admin"))
        .expect("seeded admin")
        .id
}

fn nfo(media: &Media, tv: bool) -> String {
    if tv {
        format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<episodedetails><title>{}</title><year>{}</year><plot>fixture</plot></episodedetails>\n",
            media.title,
            media.year.unwrap_or(2020)
        )
    } else {
        format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<movie><title>{}</title><year>{}</year><plot>fixture</plot></movie>\n",
            media.title,
            media.year.unwrap_or(2020)
        )
    }
}

/// Minimal but real JPEG bytes (SOI + APP0 + EOI) so image paths are exercised.
const JPEG: &[u8] = &[
    0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, b'J', b'F', b'I', b'F', 0x00, 0x01, 0x01, 0x00, 0x00, 0x01,
    0x00, 0x01, 0x00, 0x00, 0xFF, 0xD9,
];

fn open_state(root: &Path) -> ApiState {
    ApiState::new(
        Store::open(root.join("data")).unwrap(),
        TOKEN.into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(root.join("stage"))),
        root.join("library"),
    )
    .unwrap()
}

/// One media: real files + NFO + poster + ledger row(s) + one playback log.
#[allow(clippy::too_many_arguments)]
fn seed_media(
    store: &Store,
    rng: &mut Lcg,
    index: usize,
    kind: MediaKind,
    tv: bool,
    primary_root: &Path,
    second_root: &Path,
    admin: UserId,
    now: i64,
) {
    let media = Media {
        id: MediaId::new(),
        kind,
        title: format!("Perf {index:06}"),
        year: Some(2000 + (index % 25) as u16),
        original_title: None,
        tmdb_id: Some(format!("{}", 100_000 + index)),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    // 「同一 Media 多文件」+「多 Library」两种形状。
    let files = 1 + (rng.next() % 2) as usize;
    let dir = if index % 3 == 0 {
        second_root.join(format!("show-{index:06}"))
    } else {
        primary_root.join(format!("title-{index:06}"))
    };
    std::fs::create_dir_all(&dir).unwrap();
    for file_index in 0..files {
        let path = dir.join(format!("file-{file_index}.mkv"));
        std::fs::write(&path, b"perf-fixture-payload").unwrap();
        std::fs::write(dir.join(format!("file-{file_index}.nfo")), nfo(&media, tv)).unwrap();
        store
            .insert_ledger(&LedgerRow {
                id: LedgerId::new(),
                media_id: media.id,
                path: path.display().to_string(),
                season: tv.then_some(1),
                episode: tv.then_some(file_index as u32 + 1),
                resolution: Some("1080p".into()),
                codec: Some("h264".into()),
                hdr: None,
                quality_source: QualitySource::Release,
                confidence: Confidence::High,
                filter_score: None,
            })
            .unwrap();
    }
    std::fs::write(dir.join("poster.jpg"), JPEG).unwrap();

    let device = format!("dev-{index}");
    let started = now - (index as i64 % 30) * 86_400 - 60;
    store
        .upsert_session(&SessionRow {
            device_id: device.clone(),
            user_id: admin,
            media_id: media.id,
            season: tv.then_some(1),
            episode: tv.then_some(1),
            client: Some("perf-client".into()),
            device_name: None,
            client_version: None,
            play_method: "local".into(),
            position_ms: 30_000,
            start_position_ms: 0,
            duration_ms: Some(1_800_000),
            paused: false,
            watched_ms: 30_000,
            rate_bps: 0,
            bytes_sent: 0,
            connections: 1,
            admin_ended: false,
            started_at: started,
            last_report_at: started,
            ledger_id: None,
            play_session_id: None,
            last_report_at_ms: started * 1000,
            last_event_sequence: None,
        })
        .unwrap();
    store.close_session(admin, &device, started + 1).unwrap();
}

/// 非空 catalog cache（TMDB 客户端会读它，避免真实网络）。
fn seed_catalog_cache(store: &Store, now: i64) {
    for index in 0..8 {
        store
            .put_catalog_cache(
                "tmdb",
                &format!("perf-cache-{index}"),
                r#"{"page":1,"results":[]}"#,
                now,
                None,
            )
            .unwrap();
    }
}

/// Build `count` media with real files/NFO/posters plus playback logs and a
/// non-empty catalog cache. Returns `(tempdir, state, router, primary library id)`.
fn fixture(count: usize, mode: &str) -> (tempfile::TempDir, ApiState, axum::Router, String) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let tv = mode == "tv";
    let kind = if tv { MediaKind::Tv } else { MediaKind::Movie };
    // ApiState 启动时会 seed admin 用户，fixture 需要它的 id 来写 playback logs。
    let state = open_state(&root);
    let store = state.store();
    let store = store.lock();
    let admin = admin_user(&store);

    let primary_root = root.join("media").join(mode);
    let second_root = root.join("media").join(format!("{mode}-second"));
    std::fs::create_dir_all(&primary_root).unwrap();
    std::fs::create_dir_all(&second_root).unwrap();
    let primary = store
        .create_library(
            kind,
            "Perf primary",
            &[primary_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    store
        .create_library(
            kind,
            "Perf second",
            &[second_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();

    let now = unix_now();
    let mut rng = Lcg(SEED);
    for index in 0..count {
        seed_media(
            &store,
            &mut rng,
            index,
            kind,
            tv,
            &primary_root,
            &second_root,
            admin,
            now,
        );
    }
    seed_catalog_cache(&store, now);

    let library_id = primary.id.clone();
    drop(store);
    let app = router(state.clone());
    (tmp, state, app, library_id)
}

fn endpoint_uri(endpoint: &str, library_id: &str) -> String {
    match endpoint {
        "items" => format!("/api/v1/libraries/{library_id}/items?limit=24"),
        "gallery" => format!("/api/v1/libraries/{library_id}/gallery?limit=24"),
        "facets" => format!("/api/v1/libraries/{library_id}/facets"),
        "search" => "/api/v1/search/library-items?keyword=Perf".to_string(),
        "history" => "/api/v1/playback/history?limit=30".to_string(),
        "stats" => "/api/v1/playback/stats/watch?days=30".to_string(),
        "jobs" => "/api/v1/jobs".to_string(),
        other => panic!("unknown endpoint {other}"),
    }
}

struct Sample {
    status: u16,
    bytes: usize,
    elapsed: Duration,
}

async fn measure(app: &axum::Router, uri: &str, samples: usize) -> Vec<Sample> {
    let mut out = Vec::with_capacity(samples);
    for _ in 0..samples {
        let request = Request::builder()
            .method("GET")
            .uri(uri)
            .header("authorization", format!("Bearer {TOKEN}"))
            .body(Body::empty())
            .unwrap();
        let start = Instant::now();
        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status().as_u16();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap().len();
        out.push(Sample {
            status,
            bytes,
            elapsed: start.elapsed(),
        });
    }
    out
}

fn percentile(sorted: &[Duration], fraction: f64) -> Duration {
    let index = (((sorted.len() - 1) as f64) * fraction).round() as usize;
    sorted[index]
}

fn env_list(name: &str, default: &[usize]) -> Vec<usize> {
    match std::env::var(name) {
        Ok(raw) => raw
            .split(',')
            .filter_map(|value| value.trim().parse::<usize>().ok())
            .collect(),
        Err(_) => default.to_vec(),
    }
}

fn env_modes(default: &[&str]) -> Vec<String> {
    match std::env::var("PERF_MODES") {
        Ok(raw) => raw.split(',').map(|value| value.trim().to_string()).collect(),
        Err(_) => default.iter().map(|value| (*value).to_string()).collect(),
    }
}

fn env_endpoints(default: &[&'static str]) -> Vec<String> {
    match std::env::var("PERF_ENDPOINTS") {
        Ok(raw) => raw
            .split(',')
            .map(|value| {
                let trimmed = value.trim();
                if !default.iter().any(|&d| d == trimmed) {
                    panic!("unknown endpoint in PERF_ENDPOINTS: {trimmed}");
                }
                trimmed.to_string()
            })
            .collect(),
        Err(_) => default.iter().map(|value| (*value).to_string()).collect(),
    }
}

fn print_row(mode: &str, size: usize, endpoint: &str, samples: &[Sample]) {
    let mut times: Vec<Duration> = samples.iter().map(|sample| sample.elapsed).collect();
    times.sort();
    let bytes = samples.first().map(|sample| sample.bytes).unwrap_or(0);
    let status = samples.first().map(|sample| sample.status).unwrap_or(0);
    let all_ok = samples.iter().all(|sample| sample.status == status);
    println!(
        "{mode}\t{size}\t{endpoint}\t{status}{}\t{bytes}\t{:.3}\t{:.3}\t{}",
        if all_ok { "" } else { "!" },
        percentile(&times, 0.5).as_secs_f64() * 1_000.0,
        percentile(&times, 0.95).as_secs_f64() * 1_000.0,
        samples.len(),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "manual release comparison"]
async fn endpoint_latency_matrix() {
    let sizes = env_list("PERF_SIZES", DEFAULT_SIZES);
    let modes = env_modes(DEFAULT_MODES);
    let endpoints = env_endpoints(ENDPOINTS);
    let samples = env_list("PERF_SAMPLES", &[DEFAULT_SAMPLES])
        .first()
        .copied()
        .unwrap_or(DEFAULT_SAMPLES)
        .max(1);
    println!("mode\tsize\tendpoint\tstatus\tbytes\tmedian_ms\tp95_ms\tsamples");
    for mode in modes {
        for size in &sizes {
            let (_tmp, _state, app, library_id) = fixture(*size, &mode);
            for endpoint in &endpoints {
                let uri = endpoint_uri(endpoint, &library_id);
                let measured = measure(&app, &uri, samples).await;
                print_row(&mode, *size, endpoint, &measured);
            }
        }
    }
}

/// Smoke test for the fixture itself: keeps `cargo test -p api` fast and
/// verifies the data shape without measuring anything.
#[tokio::test]
async fn fixture_serves_every_measured_endpoint() {
    let (_tmp, _state, app, library_id) = fixture(25, "movie");
    for endpoint in ENDPOINTS {
        let uri = endpoint_uri(endpoint, &library_id);
        let measured = measure(&app, &uri, 1).await;
        assert_eq!(measured[0].status, 200, "{endpoint} must answer 200");
    }
    let (_tmp, _state, app, library_id) = fixture(25, "tv");
    for endpoint in ENDPOINTS {
        let uri = endpoint_uri(endpoint, &library_id);
        let measured = measure(&app, &uri, 1).await;
        assert_eq!(measured[0].status, 200, "tv {endpoint} must answer 200");
    }
}
