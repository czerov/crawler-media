use super::*;
use domain::{Confidence, LedgerId, QualitySource};

fn unit() -> ProbeUnit {
    ProbeUnit {
        row: LedgerRow {
            id: LedgerId::new(),
            media_id: MediaId::new(),
            path: "deleted.strm".into(),
            season: Some(1),
            episode: Some(1),
            resolution: None,
            codec: None,
            hdr: None,
            quality_source: QualitySource::Release,
            confidence: Confidence::High,
            filter_score: None,
        },
        kind: MediaKind::Tv,
        force_fingerprint: false,
        reuse_fingerprint_cache: false,
        overwrite_markers: false,
        reuse_media_info_cache: true,
        marker_refresh_id: None,
        job_id: None,
    }
}

#[test]
fn missing_metadata_job_stops_fingerprint_enqueue_without_deadlock() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(Mutex::new(Store::open(tmp.path()).unwrap()));
    let manager = Arc::new(ProbeManager::new(store.clone()));
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut unit = unit();
        unit.job_id = Some("missing-job".into());
        let work = probe::FingerprintWork {
            unit,
            media_duration_ms: None,
            source_version: "deleted".into(),
            duration_secs: 60,
            start_job: false,
        };
        tx.send(manager.enqueue_fingerprint_after_metadata(work))
            .unwrap();
    });
    assert!(
        !rx.recv_timeout(Duration::from_secs(3))
            .expect("missing-job handling must release Store before marking a probe failed",)
    );
    assert!(store.try_lock().is_some());
}

#[test]
fn deleted_probe_cancellation_and_detail_enqueue_remain_responsive() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(Mutex::new(Store::open(tmp.path()).unwrap()));
    let manager = Arc::new(ProbeManager::new(store.clone()));
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    for cancel in [false, true] {
        let manager = manager.clone();
        let barrier = barrier.clone();
        let done = done_tx.clone();
        std::thread::spawn(move || {
            for _ in 0..1000 {
                let request = unit();
                assert!(manager.enqueue(request));
                let queued = manager.take_queued_for_test().unwrap();
                barrier.wait();
                if cancel {
                    manager.cancel_unit(&queued, "file_deleted");
                } else {
                    assert!(manager.enqueue(unit()));
                    let extra = manager.take_queued_for_test().unwrap();
                    manager.finish(&extra, true);
                    manager.finish(&queued, true);
                }
                barrier.wait();
            }
            done.send(()).unwrap();
        });
    }
    for _ in 0..2 {
        done_rx
            .recv_timeout(Duration::from_secs(20))
            .expect("deleted-file cancellation and detail enqueue must not deadlock the Store");
    }
    assert!(
        store.try_lock().is_some(),
        "authentication must retain Store access"
    );
}
