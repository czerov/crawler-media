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
        original_title: None, aliases: Vec::new(), tmdb_id: None, douban_id: None, tvdb_id: None, bangumi_id: None, anilist_id: None,
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
            15
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
        original_title: None, aliases: Vec::new(), tmdb_id: None, douban_id: None, tvdb_id: None,
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

fn closed_session(
    user: UserId,
    media: MediaId,
    device: &str,
    started: i64,
    admin_ended: bool,
) -> store::SessionRow {
    store::SessionRow {
        device_id: device.into(),
        user_id: user,
        media_id: media,
        season: None,
        episode: None,
        client: Some("fixture".into()),
        device_name: None,
        client_version: None,
        play_method: "local".into(),
        position_ms: 500,
        start_position_ms: 0,
        duration_ms: Some(1000),
        paused: false,
        watched_ms: 10,
        rate_bps: 0,
        bytes_sent: 0,
        connections: 0,
        admin_ended,
        started_at: started,
        last_report_at: started,
        ledger_id: None,
        play_session_id: None,
        last_report_at_ms: started * 1000,
        last_event_sequence: None,
    }
}

/// 只有管理员结束的会话才写 `admin_ended = 1`；用户自行停止必须是 0，
/// 否则迟到的心跳会让前端弹出"管理员已结束播放"。
#[test]
fn only_admin_end_records_admin_ended_in_playback_log() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let user = UserId::new();
    let media = MediaId::new();

    store
        .upsert_session(&closed_session(user, media, "dev-user-stop", 10, false))
        .unwrap();
    store.close_session(user, "dev-user-stop", 20).unwrap();
    store
        .upsert_session(&closed_session(user, media, "dev-admin-end", 30, true))
        .unwrap();
    store.close_session(user, "dev-admin-end", 40).unwrap();

    let db = Connection::open(tmp.path().join("subscribe.db")).unwrap();
    let user_stop: i64 = db
        .query_row(
            "SELECT admin_ended FROM playback_logs WHERE device_id = 'dev-user-stop'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let admin_end: i64 = db
        .query_row(
            "SELECT admin_ended FROM playback_logs WHERE device_id = 'dev-admin-end'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(user_stop, 0, "用户自行停止不得被记为管理员结束");
    assert_eq!(admin_end, 1, "管理员结束必须落库");

    assert!(
        !store.device_was_admin_ended(user, "dev-user-stop", 25, 60).unwrap(),
        "用户停止后的迟到心跳不得被判定为管理员结束"
    );
    assert!(
        store.device_was_admin_ended(user, "dev-admin-end", 45, 60).unwrap(),
        "管理员结束后的心跳必须被告知已结束"
    );
}

/// 老库没有 `admin_ended` 列：重开 Store 必须幂等地补列，且默认 0。
#[test]
fn reopening_legacy_db_restores_admin_ended_column_with_default_zero() {
    let tmp = tempfile::tempdir().unwrap();
    drop(Store::open(tmp.path()).unwrap());
    let user = UserId::new();
    let media = MediaId::new();
    {
        let db = Connection::open(tmp.path().join("subscribe.db")).unwrap();
        db.execute_batch("ALTER TABLE playback_logs DROP COLUMN admin_ended;")
            .unwrap();
        db.execute(
            "INSERT INTO playback_logs (id,user_id,media_id,play_method,started_at,ended_at)
             VALUES ('legacy',?1,?2,'local',1,2)",
            params![user.to_string(), media.to_string()],
        )
        .unwrap();
    }

    for _ in 0..2 {
        let reopened = Store::open(tmp.path()).unwrap();
        let logs = reopened
            .list_logs(10, None, None, None, 10, Some(user))
            .unwrap();
        assert_eq!(logs.len(), 1);
        assert!(
            !reopened
                .device_was_admin_ended(user, "jf-legacy", 3, 60)
                .unwrap(),
            "老日志不得被当成管理员结束"
        );
    }

    let db = Connection::open(tmp.path().join("subscribe.db")).unwrap();
    let value: i64 = db
        .query_row(
            "SELECT admin_ended FROM playback_logs WHERE id = 'legacy'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(value, 0, "补列后老行默认 0");
}
