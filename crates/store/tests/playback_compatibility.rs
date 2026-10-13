use std::str::FromStr;

use domain::{MediaId, UserId};
use rusqlite::{Connection, params};
use store::Store;

#[test]
fn old_play_logs_gain_nullable_identity_without_inventing_devices() {
    let tmp = tempfile::tempdir().unwrap();
    drop(Store::open(tmp.path()).unwrap());
    let user = UserId::new();
    let media = MediaId::new();
    let db = Connection::open(tmp.path().join("subscribe.db")).unwrap();
    db.execute_batch("ALTER TABLE playback_logs DROP COLUMN device_id; PRAGMA user_version=2;")
        .unwrap();
    db.execute(
        "INSERT INTO playback_logs (id,user_id,media_id,client,device_name,play_method,started_at,ended_at)
         VALUES ('old',?1,?2,'Infuse','Same Name','DirectPlay',1,2)",
        params![user.to_string(), media.to_string()],
    ).unwrap();
    for _ in 0..2 {
        let reopened = Store::open(tmp.path()).unwrap();
        let logs = reopened
            .list_logs(10, None, None, None, 10, Some(user))
            .unwrap();
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].device_id, None);
        assert!(!logs[0].revocable());
        assert!(!reopened.device_is_revocable(user, "jf-Same Name").unwrap());
    }
}

#[test]
fn failed_movie_mark_does_not_hide_legacy_progress() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let user = UserId::from_str("00000000-0000-0000-0000-000000000001").unwrap();
    let media = MediaId::new();
    store.insert_media(&domain::Media {
        id: media, kind: domain::MediaKind::Movie, title: "Legacy".into(), year: None,
        original_title: None, tmdb_id: None, douban_id: None, tvdb_id: None, bangumi_id: None, anilist_id: None,
    }).unwrap();
    store.upsert_unit(user, media, 0, 0, 45000, Some(false), Some(true), Some(90000), None, None, false, 10).unwrap();
    let db = Connection::open(tmp.path().join("subscribe.db")).unwrap();
    db.execute_batch("CREATE TRIGGER review_mark_update_block BEFORE UPDATE ON playback_units BEGIN SELECT RAISE(ABORT,'review mark blocked'); END;").unwrap();
    drop(db);
    let blocked = Store::open(tmp.path()).unwrap();
    assert!(blocked.set_unit_marks(user, media, -1, -1, Some(true), None, 20).is_err());
    drop(blocked);
    let reopened = Store::open(tmp.path()).unwrap();
    let legacy = reopened.unit_state(user, media, 0, 0).unwrap().unwrap();
    assert_eq!(legacy.position_ms, 45000);
    assert!(legacy.favorite);
    assert!(reopened.unit_state(user, media, -1, -1).unwrap().is_none());
}

#[test]
fn old_library_gains_nullable_release_quality_idempotently() {
    let tmp = tempfile::tempdir().unwrap();
    drop(Store::open(tmp.path()).unwrap());
    let db = Connection::open(tmp.path().join("library.db")).unwrap();
    db.execute_batch("ALTER TABLE ledger DROP COLUMN release_quality; PRAGMA user_version=5;")
        .unwrap();
    let media = MediaId::new();
    db.execute("INSERT INTO ledger (id,media_id,path,quality_source,confidence) VALUES ('old',?1,'old.mkv','probe','high')",[media.to_string()]).unwrap();
    for _ in 0..2 {
        let reopened = Store::open(tmp.path()).unwrap();
        assert_eq!(
            reopened
                .schema_versions()
                .into_iter()
                .find(|(name, _)| *name == "library")
                .unwrap()
                .1,
            9
        );
        let quality: Option<String> = db
            .query_row(
                "SELECT release_quality FROM ledger WHERE id='old'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(quality, None);
    }
}

#[test]
fn clear_viewing_facts_preserves_preferences_and_other_users_and_time_windows() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let user = UserId::from_str("00000000-0000-0000-0000-000000000001").unwrap();
    let other = UserId::new();
    let media = MediaId::new();
    let outside = MediaId::new();
    for (who, what, at) in [(user, media, 100), (other, media, 100), (user, outside, 10)] {
        store
            .upsert_unit(
                who,
                what,
                1,
                1,
                1000,
                Some(true),
                Some(true),
                Some(120000),
                Some("embedded:1"),
                Some("off"),
                true,
                at,
            )
            .unwrap();
    }
    assert_eq!(store.clear_units(user, None, Some(50)).unwrap(), 1);
    let cleared = store.unit_state(user, media, 1, 1).unwrap().unwrap();
    assert_eq!(
        (cleared.position_ms, cleared.played, cleared.play_count),
        (0, false, 0)
    );
    assert_eq!(
        (
            cleared.favorite,
            cleared.audio_track.as_deref(),
            cleared.subtitle_track.as_deref()
        ),
        (true, Some("embedded:1"), Some("off"))
    );
    assert_eq!(
        store
            .unit_state(other, media, 1, 1)
            .unwrap()
            .unwrap()
            .position_ms,
        1000
    );
    assert_eq!(
        store
            .unit_state(user, outside, 1, 1)
            .unwrap()
            .unwrap()
            .position_ms,
        1000
    );
    assert_eq!(
        store.user_active_media_ids(user).unwrap(),
        vec![(outside, 10)]
    );
}

#[test]
fn critical_unit_mark_cascade_failure_is_propagated_and_atomic() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let user = UserId::new();
    let media = MediaId::new();
    for (season, episode) in [(-1, -1), (1, 1)] {
        store
            .upsert_unit(
                user,
                media,
                season,
                episode,
                0,
                None,
                Some(true),
                None,
                None,
                None,
                false,
                1,
            )
            .unwrap();
    }
    let db = Connection::open(tmp.path().join("subscribe.db")).unwrap();
    db.execute_batch("CREATE TRIGGER block_mark BEFORE UPDATE ON playback_units WHEN OLD.season=1 BEGIN SELECT RAISE(ABORT,'mark blocked'); END;").unwrap();
    assert!(
        store
            .set_unit_marks(user, media, -1, -1, None, Some(false), 2)
            .is_err()
    );
    for (season, episode) in [(-1, -1), (1, 1)] {
        assert!(
            store
                .unit_state(user, media, season, episode)
                .unwrap()
                .unwrap()
                .favorite
        );
    }
}

#[test]
fn retargeted_subscribe_does_not_treat_previous_library_as_owned() {
    use domain::{Confidence, LedgerId, Media, MediaKind, QualitySource};
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let media = domain::MediaId::new();
    store.insert_media(&Media {
        id: media, kind: MediaKind::Movie, title: "Shared".into(), year: None,
        original_title: None, tmdb_id: None, douban_id: None, tvdb_id: None,
        bangumi_id: None, anilist_id: None,
    }).unwrap();
    let a_root = tmp.path().join("library-a");
    let b_root = tmp.path().join("library-b");
    std::fs::create_dir_all(&a_root).unwrap();
    std::fs::create_dir_all(&b_root).unwrap();
    let a = store.create_library(MediaKind::Movie, "A", &[a_root.to_str().unwrap()], "everyone", true, &[]).unwrap();
    let b = store.create_library(MediaKind::Movie, "B", &[b_root.to_str().unwrap()], "everyone", true, &[]).unwrap();
    store.set_default_library(&a.id).unwrap();
    let path = a_root.join("Shared.mkv");
    std::fs::write(&path, b"owned").unwrap();
    store.insert_ledger(&domain::LedgerRow {
        id: LedgerId::new(), media_id: media, path: path.display().to_string(),
        season: None, episode: None, resolution: Some("2160p".into()), codec: None, hdr: None,
        quality_source: QualitySource::Probe, confidence: Confidence::High, filter_score: Some(100),
    }).unwrap();
    let mut subscribe = domain::Subscribe {
        id: domain::SubscribeId::new(), user_id: domain::UserId::new(), media_id: media,
        coverage: domain::Coverage::Movie, fetch_mode: domain::FetchMode::Search,
        filter_id: domain::FilterId::new(), wash_cut: false, wash_cut_filter_id: None,
        keep_old_versions: false, full_season_pack: false, downloader_id: None,
        library_id: Some(domain::LibraryId::from_str(&a.id).unwrap()),
        tracking_state: "active".into(), follow_future: false, search_interval_secs: 1800,
    };
    let mut facts = subscribe::SubscribeFacts::default();
    facts.replace(None, None, subscribe::QualityFact { score: 100, path: Some(path.display().to_string()) });
    store.save_subscribe_facts(subscribe.id, &facts).unwrap();
    subscribe.library_id = Some(domain::LibraryId::from_str(&b.id).unwrap());
    store.update_subscribe(&subscribe).unwrap();
    let loaded = store.load_library_subscribe_facts(&subscribe, MediaKind::Movie).unwrap();
    assert!(loaded.movie().is_none(), "Library A 的文件不能算作空 Library B 的已拥有内容");
    assert!(path.is_file(), "切换目标不得删除原 Library 文件");
}
