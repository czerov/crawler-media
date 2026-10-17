use api::probe_manager::{ProbeManager, ProbeUnit};
use api::{ScrapeStoreExt, Store};
use domain::{Confidence, LedgerId, LedgerRow, MediaId, MediaKind, QualitySource};
use marker::fingerprint::{
    CaptureFailure, CaptureMetrics, CaptureRequest, CapturedFingerprint, FingerprintCaptureEngine,
};
use marker::{CommonSegment, FingerprintEngine};
use parking_lot::Mutex;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

struct FixtureEngine {
    case: &'static str,
    reads: Arc<AtomicUsize>,
}

fn intro_tag(case: &str, episode: u32) -> u32 {
    if case == "blind-seeds" && [2, 4, 7].contains(&episode) {
        1000 + episode
    } else if (case == "other-variant" || case == "edge-variant") && [2, 4, 7].contains(&episode) {
        1000
    } else {
        100
    }
}

impl FingerprintEngine for FixtureEngine {
    fn extract_at(&self, _: &std::path::Path, _: u32, _: u32) -> Result<Vec<u32>, String> {
        panic!("this reproduction must not use legacy media extraction")
    }
    fn find_common_segment(
        &self,
        first: &[u32],
        second: &[u32],
        _: f32,
        _: f32,
    ) -> Option<CommonSegment> {
        if ["vote-duplication", "ambiguous-final"].contains(&self.case)
            && first.first() != Some(&5000)
            && second.first() != Some(&5000)
        {
            let other = if first.first() == Some(&104) {
                second[0]
            } else {
                first[0]
            };
            let target_start = if other == 101 || other == 105 {
                10.0
            } else {
                100.0
            };
            let start1 = if first.first() == Some(&104) {
                target_start
            } else {
                10.0
            };
            let start2 = if second.first() == Some(&104) {
                target_start
            } else {
                10.0
            };
            if start1 + 60.0 > first.len() as f32 || start2 + 60.0 > second.len() as f32 {
                return None;
            }
            return Some(CommonSegment {
                start1_sec: start1,
                end1_sec: start1 + 60.0,
                start2_sec: start2,
                end2_sec: start2 + 60.0,
                duration_sec: 60.0,
                score: 1.0,
            });
        }
        if self.case == "version-conflict"
            && first.first() != Some(&5000)
            && second.first() != Some(&5000)
        {
            let start1 = if first.first() == Some(&1200) {
                30.0
            } else {
                10.0
            };
            let start2 = if second.first() == Some(&1200) {
                30.0
            } else {
                10.0
            };
            return Some(CommonSegment {
                start1_sec: start1,
                end1_sec: start1 + 60.0,
                start2_sec: start2,
                end2_sec: start2 + 60.0,
                duration_sec: 60.0,
                score: 1.0,
            });
        }
        if first.first() != second.first()
            || self.case == "no-outro" && first.first() == Some(&5000)
        {
            return None;
        }
        let end = if self.case == "edge-variant" && first.first() == Some(&100) {
            180.0
        } else {
            70.0
        };
        Some(CommonSegment {
            start1_sec: 10.0,
            end1_sec: end,
            start2_sec: 10.0,
            end2_sec: end,
            duration_sec: end - 10.0,
            score: 1.0,
        })
    }
    fn find_common_segments(
        &self,
        first: &[u32],
        second: &[u32],
        min: f32,
        max: f32,
    ) -> Vec<CommonSegment> {
        self.find_common_segment(first, second, min, max)
            .into_iter()
            .collect()
    }
}

impl FingerprintCaptureEngine for FixtureEngine {
    fn capture_window(
        &self,
        request: &CaptureRequest,
    ) -> Result<CapturedFingerprint, CaptureFailure> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        let stem = request.path.file_stem().unwrap().to_str().unwrap();
        let episode: u32 = stem.split('-').nth(1).unwrap().parse().unwrap();
        let version: usize = stem.rsplit('-').next().unwrap().parse().unwrap();
        let tag = if request.window.start_ms == 0 {
            if ["vote-duplication", "ambiguous-final"].contains(&self.case) {
                100 + episode
            } else if (self.case == "version-conflict" || self.case == "version-missing")
                && episode == 1
                && version == 3
            {
                1200
            } else if self.case == "mixed-versions" {
                if episode == 4 || (episode == 1 && version == 3) {
                    200
                } else {
                    100
                }
            } else {
                intro_tag(self.case, episode)
            }
        } else if self.case == "version-missing" && episode == 1 && version == 3 {
            6200
        } else {
            5000
        };
        Ok(CapturedFingerprint {
            window: request.window.clone(),
            words: (tag..tag
                + if ["vote-duplication", "ambiguous-final"].contains(&self.case) {
                    (request.window.duration_ms() / 1000) as u32
                } else {
                    200
                })
                .collect(),
            pcm_duration_ms: Some(request.window.duration_ms()),
            metrics: CaptureMetrics::default(),
        })
    }
}

struct Fixture {
    _tmp: tempfile::TempDir,
    store: Arc<Mutex<Store>>,
    manager: ProbeManager,
    media_id: MediaId,
    units: Vec<ProbeUnit>,
    engine: Arc<FixtureEngine>,
}

fn setup(case: &'static str, episodes: &[u32]) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(Mutex::new(Store::open(tmp.path()).unwrap()));
    let media_id = MediaId::new();
    let engine = Arc::new(FixtureEngine {
        case,
        reads: Arc::new(AtomicUsize::new(0)),
    });
    let manager = ProbeManager::with_engines(store.clone(), engine.clone(), engine.clone());
    let mut rows: Vec<_> = episodes
        .iter()
        .enumerate()
        .map(|(index, &episode)| LedgerRow {
            id: LedgerId::new(),
            media_id,
            path: tmp
                .path()
                .join(format!("episode-{episode}-version-{index}.mkv"))
                .to_string_lossy()
                .to_string(),
            season: Some(1),
            episode: Some(episode),
            resolution: None,
            codec: None,
            hdr: None,
            quality_source: QualitySource::Release,
            confidence: Confidence::High,
            filter_score: None,
        })
        .collect();
    if (case == "mixed-versions" || case == "version-conflict" || case == "version-missing")
        && rows[0].id.to_string() > rows[3].id.to_string()
    {
        let original = rows[0].id;
        rows[0].id = rows[3].id;
        rows[3].id = original;
    }
    seed_store(&store, media_id, &rows);
    let units = rows
        .into_iter()
        .map(|row| ProbeUnit {
            row,
            kind: MediaKind::Tv,
            force_fingerprint: true,
            reuse_fingerprint_cache: false,
            overwrite_markers: true,
            reuse_media_info_cache: true,
            marker_refresh_id: Some(1),
            job_id: Some("review9-job".into()),
        })
        .collect();
    Fixture {
        _tmp: tmp,
        store,
        manager,
        media_id,
        units,
        engine,
    }
}

fn raw_season(f: &Fixture) -> Result<api::store::MarkerResultReplacement, String> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(api::probe_manager::season::run_adaptive_season_pipeline(
            &f.manager,
            "review9-job",
            f.media_id,
            1,
            &f.units,
        ))
}

fn seed_store(store: &Arc<Mutex<Store>>, media_id: MediaId, rows: &[LedgerRow]) {
    let st = store.lock();
    let mut config = st.get_scrape_config().unwrap().setting;
    config.fingerprint_sampling_mode = Some("adaptive".into());
    st.save_scrape_config(&config).unwrap();
    st.insert_media(&domain::Media {
        id: media_id,
        kind: MediaKind::Tv,
        title: "Review fixture".into(),
        year: None,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    })
    .unwrap();
    for row in rows {
        std::fs::write(&row.path, b"dummy").unwrap();
        st.insert_ledger(row).unwrap();
        st.put_file_meta_versioned(
            &row.id.to_string(),
            &library::Tracks {
                video: Some(library::VideoTrack::default()),
                audio: vec![],
                subtitles: vec![],
            },
            Some(&api::fingerprint_job::current_source_version(
                std::path::Path::new(&row.path),
            )),
            Some(1_000_000),
        )
        .unwrap();
        st.put_media_marker(&api::store::StoredMediaMarker {
            media_id,
            season: 1,
            episode: row.episode.unwrap(),
            intro_start_ms: Some(10_000),
            intro_end_ms: Some(70_000),
            outro_start_ms: None,
            outro_end_ms: None,
            source: "old".into(),
            locked: false,
            updated_at: 0,
        })
        .unwrap();
    }
    let keys: Vec<_> = rows.iter().map(|row| row.id.to_string()).collect();
    let specs: Vec<_> = keys
        .iter()
        .map(|key| api::store::ProbeJobUnitSpec {
            ledger_id: key,
            kind: "tv",
            force_fingerprint: true,
            reuse_fingerprint_cache: false,
            overwrite_markers: true,
            reuse_media_info_cache: true,
        })
        .collect();
    st.create_probe_job(
        "review9-job",
        "marker_refresh",
        &media_id.to_string(),
        Some(1),
        "review9",
        &specs,
    )
    .unwrap();
    for key in &keys {
        st.start_probe_unit("review9-job", key).unwrap();
        st.finish_probe_unit("review9-job", key, true, None)
            .unwrap();
    }
}
