//! S1: a wash-cut replacement whose downloaded file cannot be probed must not
//! delete the owned file on the strength of the torrent title alone.

use domain::{Confidence, LedgerId, LedgerRow, QualitySource, SubscribeId};

use super::harness::*;

async fn wash_subscription_owning_1080p(h: &Harness) -> (SubscribeId, std::path::PathBuf) {
    h.create_site().await;
    let id = h.create_movie_subscribe(serde_json::json!({ "wash_cut": true })).await;
    let media_id = h.state.store().lock().get_subscribe(id).unwrap().unwrap().media_id;
    let owned = h.movie_library_root().join("The Matrix (1999)").join("The Matrix (1999) - 1080p.mkv");
    std::fs::create_dir_all(owned.parent().unwrap()).unwrap();
    std::fs::write(&owned, probed_video(1080, "h264", false)).unwrap();
    let row = LedgerRow {
        id: LedgerId::new(),
        media_id,
        path: owned.display().to_string(),
        season: None,
        episode: None,
        resolution: Some("1080p".into()),
        codec: Some("h264".into()),
        hdr: None,
        quality_source: QualitySource::Probe,
        confidence: Confidence::High,
        filter_score: Some(1),
    };
    let mut facts = subscribe::SubscribeFacts::default();
    facts.replace(None, None, subscribe::QualityFact { score: 1, path: Some(row.path.clone()) });
    facts.set_quality(row.path.clone(), release::parse("The.Matrix.1999.1080p.WEB-DL.x264"));
    {
        let store = h.state.store();
        let store = store.lock();
        store.insert_ledger(&row).unwrap();
        store.save_subscribe_facts(id, &facts).unwrap();
    }
    (id, owned)
}

fn owned_fact_path(h: &Harness, id: SubscribeId) -> Option<String> {
    h.state.store().lock().load_subscribe_facts(id).unwrap().movie().and_then(|f| f.path.clone())
}

#[tokio::test]
async fn unprobeable_upgrade_keeps_the_owned_file() {
    let h = Harness::new();
    let (id, owned) = wash_subscription_owning_1080p(&h).await;

    h.run_subscribe(id).await;
    assert_eq!(h.qb.add_count(), 1, "2160p title is an upgrade candidate");
    h.qb.complete(0, MATRIX_TITLE, &[(&format!("{MATRIX_TITLE}.mkv"), &unprobeable_video())]);
    h.transfer(id).await.unwrap();

    assert!(owned.is_file(), "owned 1080p must not be deleted without a successful probe");
    assert_eq!(owned_fact_path(&h, id).as_deref(), Some(owned.display().to_string().as_str()));
    assert!(h.ledger_paths().contains(&owned.display().to_string()));

    // The unprobed arrival is not re-downloaded on the next run.
    h.run_subscribe(id).await;
    assert_eq!(h.qb.add_count(), 1);
}

#[tokio::test]
async fn probed_upgrade_still_replaces_the_owned_file() {
    let h = Harness::new();
    let (id, owned) = wash_subscription_owning_1080p(&h).await;

    h.run_subscribe(id).await;
    assert_eq!(h.qb.add_count(), 1);
    let video = probed_video(2160, "hevc", false);
    h.qb.complete(0, MATRIX_TITLE, &[(&format!("{MATRIX_TITLE}.mkv"), &video)]);
    h.transfer(id).await.unwrap();

    assert!(!owned.exists(), "probed 2160p upgrade replaces the owned 1080p");
    let files = h.library_files();
    assert_eq!(files.len(), 1, "{files:?}");
    assert_eq!(std::fs::read(&files[0]).unwrap(), video);
    assert_eq!(owned_fact_path(&h, id).as_deref(), Some(files[0].display().to_string().as_str()));
}
