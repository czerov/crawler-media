use api::Store;
use api::cli::{Command, run};
use domain::{Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource};

use super::cli::transport;

#[tokio::test]
async fn cli_library_prints_no_media_when_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let t = transport(&tmp);
    let out = run(Command::Library, &t).await.unwrap();
    assert!(out.contains("No Media"), "{out}");
}

#[tokio::test]
async fn cli_library_prints_media_title() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "The Matrix".into(),
        year: Some(1999),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    store
        .insert_ledger(&LedgerRow {
            id: LedgerId::new(),
            media_id: media.id,
            path: tmp.path().join("The Matrix.mkv").display().to_string(),
            season: None,
            episode: None,
            resolution: Some("2160p".into()),
            codec: None,
            hdr: None,
            quality_source: QualitySource::Release,
            confidence: Confidence::High,
            filter_score: None,
        })
        .unwrap();
    drop(store);
    let t = transport(&tmp);
    let out = run(Command::Library, &t).await.unwrap();
    assert!(out.contains("The Matrix"), "{out}");
    assert!(out.contains("movie"), "{out}");
}

#[tokio::test]
async fn cli_ledger_prints_no_ledger_when_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let t = transport(&tmp);
    let out = run(Command::Ledger, &t).await.unwrap();
    assert!(out.contains("No ledger"), "{out}");
}

#[tokio::test]
async fn cli_ledger_prints_media_title_and_path() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "The Matrix".into(),
        year: Some(1999),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    store
        .insert_ledger(&LedgerRow {
            id: LedgerId::new(),
            media_id: media.id,
            path: tmp.path().join("The Matrix.mkv").display().to_string(),
            season: None,
            episode: None,
            resolution: Some("2160p".into()),
            codec: None,
            hdr: None,
            quality_source: QualitySource::Release,
            confidence: Confidence::High,
            filter_score: None,
        })
        .unwrap();
    drop(store);
    let t = transport(&tmp);
    let out = run(Command::Ledger, &t).await.unwrap();
    assert!(out.contains("The Matrix"), "{out}");
    assert!(out.contains("The Matrix.mkv"), "{out}");
}

#[tokio::test]
async fn cli_unidentified_prints_no_unidentified_when_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let t = transport(&tmp);
    let out = run(Command::Unidentified, &t).await.unwrap();
    assert!(out.contains("No Unidentified"), "{out}");
}

#[tokio::test]
async fn cli_unidentified_prints_parked_path() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("unidentified/foo.mkv");
    let store = Store::open(tmp.path().join("data")).unwrap();
    store
        .insert_unidentified(path.display().to_string(), Confidence::Low)
        .unwrap();
    drop(store);
    let t = transport(&tmp);
    let out = run(Command::Unidentified, &t).await.unwrap();
    assert!(out.contains(&path.display().to_string()), "{out}");
    assert!(out.contains("low"), "{out}");
}

#[tokio::test]
async fn cli_unidentified_claim_prints_media_title() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let intake = tmp.path().join("intake");
    std::fs::create_dir_all(&intake).unwrap();
    let src = intake.join("foo.mkv");
    std::fs::write(&src, b"video").unwrap();
    store
        .insert_unidentified(src.display().to_string(), Confidence::Low)
        .unwrap();
    drop(store);
    let t = transport(&tmp);
    let out = run(
        Command::ClaimUnidentified {
            path: src.display().to_string(),
            title: "The Matrix".into(),
            kind: "movie".into(),
            year: Some(1999),
        },
        &t,
    )
    .await
    .unwrap();
    assert!(out.contains("The Matrix"), "{out}");
    let listed = run(Command::Unidentified, &t).await.unwrap();
    assert!(listed.contains("No Unidentified"), "{listed}");
}
