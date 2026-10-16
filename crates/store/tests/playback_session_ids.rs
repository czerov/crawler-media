use domain::{LedgerId, UserId};
use store::{PlaySessionBinding, PlaySessionIdState, SessionRow, Store};

#[test]
fn issued_id_is_bound_to_user_device_and_ledger() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();

    let user_a = UserId::new();
    let user_b = UserId::new();
    let ledger_id = LedgerId::new();

    let id = store
        .issue_play_session(user_a, "device_1", ledger_id, 1000)
        .unwrap();

    // Matching user, device, and id returns binding
    let binding = store
        .play_session_binding(user_a, "device_1", &id)
        .unwrap()
        .expect("binding must exist");
    assert_eq!(binding.ledger_id, ledger_id);
    assert_eq!(binding.issued_at, 1000);
    assert_eq!(binding.generation, 1);
    assert_eq!(binding.state, PlaySessionIdState::Pending);

    // Mismatched user returns None
    assert!(
        store
            .play_session_binding(user_b, "device_1", &id)
            .unwrap()
            .is_none()
    );

    // Mismatched device returns None
    assert!(
        store
            .play_session_binding(user_a, "device_2", &id)
            .unwrap()
            .is_none()
    );
}

#[test]
fn closed_or_older_pending_cannot_reactivate() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();

    let user = UserId::new();
    let ledger_a = LedgerId::new();
    let ledger_b = LedgerId::new();

    // Issue A at t = 1000, generation = 1
    let id_a = store
        .issue_play_session(user, "dev1", ledger_a, 1000)
        .unwrap();

    // Issue B at t = 1000 (same second!), generation = 2
    let id_b = store
        .issue_play_session(user, "dev1", ledger_b, 1000)
        .unwrap();

    // Activate B: this should activate B and close older pending A (generation 1 < 2)
    let ok_b = store.activate_play_session(user, "dev1", &id_b).unwrap();
    assert!(ok_b, "activating B must succeed");

    let binding_b = store
        .play_session_binding(user, "dev1", &id_b)
        .unwrap()
        .unwrap();
    assert_eq!(binding_b.state, PlaySessionIdState::Active);

    // Old pending A should now be Closed
    let binding_a = store
        .play_session_binding(user, "dev1", &id_a)
        .unwrap()
        .unwrap();
    assert_eq!(binding_a.state, PlaySessionIdState::Closed);

    // Trying to activate A now MUST FAIL
    let ok_a = store.activate_play_session(user, "dev1", &id_a).unwrap();
    assert!(!ok_a, "activating closed A must fail");
}

#[test]
fn repeated_activation_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();

    let user = UserId::new();
    let ledger = LedgerId::new();

    let id = store
        .issue_play_session(user, "dev1", ledger, 1000)
        .unwrap();
    assert!(store.activate_play_session(user, "dev1", &id).unwrap());
    // Second activation of same active session returns true (idempotent)
    assert!(store.activate_play_session(user, "dev1", &id).unwrap());

    // Explicit close
    store.close_play_session_id(user, "dev1", &id).unwrap();
    let binding = store
        .play_session_binding(user, "dev1", &id)
        .unwrap()
        .unwrap();
    assert_eq!(binding.state, PlaySessionIdState::Closed);

    // Activation after close returns false
    assert!(!store.activate_play_session(user, "dev1", &id).unwrap());
}

#[test]
fn old_schema_migrates_without_resetting_user_facts() {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    // Create subscribe.db at schema version 3
    let sub_path = data_dir.join("subscribe.db");
    {
        let conn = rusqlite::Connection::open(&sub_path).unwrap();
        conn.execute_batch(
            r#"
            PRAGMA user_version = 3;
            CREATE TABLE playback_units (
                user_id TEXT NOT NULL,
                media_id TEXT NOT NULL,
                season INTEGER NOT NULL,
                episode INTEGER NOT NULL,
                position_ms INTEGER NOT NULL DEFAULT 0,
                played INTEGER NOT NULL DEFAULT 0,
                favorite INTEGER NOT NULL DEFAULT 0,
                duration_ms INTEGER,
                audio_track TEXT,
                subtitle_track TEXT,
                play_count INTEGER NOT NULL DEFAULT 0,
                updated_at INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (user_id, media_id, season, episode)
            );
            CREATE TABLE playback_sessions (
                device_id TEXT NOT NULL,
                user_id TEXT NOT NULL,
                media_id TEXT NOT NULL,
                season INTEGER,
                episode INTEGER,
                client TEXT,
                device_name TEXT,
                client_version TEXT,
                play_method TEXT NOT NULL DEFAULT 'DirectPlay',
                position_ms INTEGER NOT NULL DEFAULT 0,
                start_position_ms INTEGER NOT NULL DEFAULT 0,
                duration_ms INTEGER,
                paused INTEGER NOT NULL DEFAULT 0,
                watched_ms INTEGER NOT NULL DEFAULT 0,
                rate_bps INTEGER NOT NULL DEFAULT 0,
                bytes_sent INTEGER NOT NULL DEFAULT 0,
                connections INTEGER NOT NULL DEFAULT 1,
                admin_ended INTEGER NOT NULL DEFAULT 0,
                started_at INTEGER NOT NULL DEFAULT 0,
                last_report_at INTEGER NOT NULL DEFAULT 0,
                ledger_id TEXT,
                PRIMARY KEY (user_id, device_id)
            );
            CREATE TABLE pending_downloads (
                id TEXT PRIMARY KEY,
                subscribe_id TEXT NOT NULL,
                state TEXT NOT NULL DEFAULT 'active'
            );
            "#,
        )
        .unwrap();

        // Insert legacy unit and session
        conn.execute(
            "INSERT INTO playback_units (user_id, media_id, season, episode, position_ms, played, favorite, updated_at)
             VALUES ('u1', 'm1', -1, -1, 750000, 0, 1, 42)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO playback_sessions (device_id, user_id, media_id, position_ms, last_report_at)
             VALUES ('dev1', 'u1', 'm1', 750000, 1234)",
            [],
        )
        .unwrap();
    }

    // Now open Store, which runs schema migration 3 -> 4
    let store = Store::open(&data_dir).unwrap();

    let user = UserId::new(); // test query with string or direct conn
    let sub_conn = rusqlite::Connection::open(&sub_path).unwrap();
    let version: i64 = sub_conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, 7, "subscribe.db schema version must be 7");

    // Check session backfilled last_report_at_ms = 1234 * 1000
    let (last_report_at_ms, play_session_id): (i64, Option<String>) = sub_conn
        .query_row(
            "SELECT last_report_at_ms, play_session_id FROM playback_sessions WHERE device_id = 'dev1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(last_report_at_ms, 1234 * 1000);
    assert_eq!(play_session_id, None);
    let favorite_at: i64 = sub_conn
        .query_row(
            "SELECT favorite_at FROM playback_units WHERE user_id = 'u1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(favorite_at, 42, "an old favourite keeps its activity as favorite_at");
    // A later playback must not be captured as the historical favourite time.
    sub_conn
        .execute(
            "UPDATE playback_units SET updated_at = 900 WHERE user_id = 'u1'",
            [],
        )
        .unwrap();
    drop(sub_conn);
    let _ = Store::open(&data_dir).unwrap();
    let sub_conn = rusqlite::Connection::open(&sub_path).unwrap();
    let favorite_at_after_playback: i64 = sub_conn
        .query_row(
            "SELECT favorite_at FROM playback_units WHERE user_id = 'u1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        favorite_at_after_playback,
        42,
        "reopening must not replace a backfilled favourite time with a later playback timestamp"
    );
}

#[test]
fn schema_six_keeps_unknown_favorite_times_while_schema_five_backfills_once() {
    let schema_six = tempfile::tempdir().unwrap();
    let schema_six_dir = schema_six.path().join("data");
    std::fs::create_dir_all(&schema_six_dir).unwrap();
    seed_pre_clock_subscribe(&schema_six_dir.join("subscribe.db"), 6, true);
    let _ = Store::open(&schema_six_dir).unwrap();
    let schema_six_conn = rusqlite::Connection::open(schema_six_dir.join("subscribe.db")).unwrap();
    let unknown: i64 = schema_six_conn
        .query_row(
            "SELECT favorite_at FROM playback_units WHERE user_id = 'unknown'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let known: i64 = schema_six_conn
        .query_row(
            "SELECT favorite_at FROM playback_units WHERE user_id = 'known'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(unknown, 0, "schema 6 already owns the clock; zero stays unknown");
    assert_eq!(known, 50, "a recorded favourite time must not move");
    let order: Vec<String> = schema_six_conn
        .prepare("SELECT user_id FROM playback_units WHERE favorite != 0 ORDER BY favorite_at DESC, user_id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(order, ["known", "unknown"], "an advanced playback clock must not reorder the unknown favourite");

    let schema_five = tempfile::tempdir().unwrap();
    let schema_five_dir = schema_five.path().join("data");
    std::fs::create_dir_all(&schema_five_dir).unwrap();
    seed_pre_clock_subscribe(&schema_five_dir.join("subscribe.db"), 5, false);
    let _ = Store::open(&schema_five_dir).unwrap();
    let schema_five_conn = rusqlite::Connection::open(schema_five_dir.join("subscribe.db")).unwrap();
    let backfilled: i64 = schema_five_conn
        .query_row(
            "SELECT favorite_at FROM playback_units WHERE user_id = 'unknown'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(backfilled, 200, "schema 5 predates the clock and gets the one-time backfill");
}

fn seed_pre_clock_subscribe(path: &std::path::Path, version: i64, with_favorite_at: bool) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(&format!("PRAGMA user_version = {version};")).unwrap();
    conn.execute_batch(
        "CREATE TABLE playback_units (
            user_id TEXT NOT NULL,
            media_id TEXT NOT NULL,
            season INTEGER NOT NULL,
            episode INTEGER NOT NULL,
            position_ms INTEGER NOT NULL DEFAULT 0,
            played INTEGER NOT NULL DEFAULT 0,
            favorite INTEGER NOT NULL DEFAULT 0,
            duration_ms INTEGER,
            audio_track TEXT,
            subtitle_track TEXT,
            play_count INTEGER NOT NULL DEFAULT 0,
            updated_at INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (user_id, media_id, season, episode)
        );
        CREATE TABLE playback_sessions (
            device_id TEXT NOT NULL,
            user_id TEXT NOT NULL,
            media_id TEXT NOT NULL,
            play_method TEXT NOT NULL DEFAULT 'DirectPlay',
            position_ms INTEGER NOT NULL DEFAULT 0,
            last_report_at INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (user_id, device_id)
        );
        CREATE TABLE pending_downloads (
            id TEXT PRIMARY KEY,
            subscribe_id TEXT NOT NULL,
            state TEXT NOT NULL DEFAULT 'active'
        );",
    )
    .unwrap();
    if with_favorite_at {
        conn.execute_batch("ALTER TABLE playback_units ADD COLUMN favorite_at INTEGER NOT NULL DEFAULT 0;")
            .unwrap();
    }
    conn.execute(
        "INSERT INTO playback_units
            (user_id, media_id, season, episode, favorite, updated_at)
         VALUES ('unknown', 'm-unknown', -1, -1, 1, 200)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO playback_units
            (user_id, media_id, season, episode, favorite, updated_at)
         VALUES ('known', 'm-known', -1, -1, 1, 80)",
        [],
    )
    .unwrap();
    if with_favorite_at {
        conn.execute("UPDATE playback_units SET favorite_at = 50 WHERE user_id = 'known'", [])
            .unwrap();
    }
}

#[test]
fn expiry_keeps_active_and_is_bounded_500() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();

    let user = UserId::new();
    let ledger = LedgerId::new();

    // Issue 501 sessions at t = 100 (older than 30 days ago from now)
    let t_old = 100;
    for _ in 0..500 {
        let id = store
            .issue_play_session(user, "dev1", ledger, t_old)
            .unwrap();
        store.close_play_session_id(user, "dev1", &id).unwrap();
    }
    let id_501 = store
        .issue_play_session(user, "dev1", ledger, t_old)
        .unwrap();
    store.close_play_session_id(user, "dev1", &id_501).unwrap();

    // Plus 1 active session at t_old
    let id_active = store
        .issue_play_session(user, "dev1", ledger, t_old)
        .unwrap();
    store.activate_play_session(user, "dev1", &id_active).unwrap();

    // Now issue a new session at t = 100_000_000 (triggering clean_expired bounded at 500)
    let now = 100_000_000;
    let _id_new = store
        .issue_play_session(user, "dev1", ledger, now)
        .unwrap();

    // Active session MUST NOT be deleted
    let active_binding = store
        .play_session_binding(user, "dev1", &id_active)
        .unwrap();
    assert!(active_binding.is_some(), "active session must not be deleted");

    // Out of 501 closed old sessions, 500 were cleaned, leaving 1 closed + 1 active + 1 new pending = 3 rows
    let sub_conn =
        rusqlite::Connection::open(tmp.path().join("data/subscribe.db")).unwrap();
    let count: i64 = sub_conn
        .query_row("SELECT COUNT(*) FROM playback_session_ids", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(count, 3, "expected 1 remaining old closed + 1 active + 1 new pending");
}
