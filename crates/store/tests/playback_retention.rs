//! DB8 Task 3：`playback_logs` 分批保留。
//!
//! 断言只走公开 seam（`upsert_session`/`close_session`/`list_logs`/
//! `playback_log_page`/`playback_watch_report`/`unit_state`/`get_session`）。
//! 唯一例外是 `playback_metrics`：store 目前没有公开的 metrics 读取接口，
//! 因此行数用 test-only 的直连只读 COUNT 断言（见 `metric_count`）。

use std::path::Path;

use domain::{MediaId, UserId};
use store::{PlaybackLogScope, SessionRow, Store};

const DAY: i64 = 86_400;

fn session(user: UserId, media: MediaId, device: &str, started: i64) -> SessionRow {
    SessionRow {
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
        admin_ended: false,
        started_at: started,
        last_report_at: started,
        ledger_id: None,
    }
}

/// 公开 seam 造一条历史：`upsert_session` + `close_session`（`now` 即 ended_at）。
fn log_fixture(
    store: &Store,
    user: UserId,
    media: MediaId,
    device: &str,
    started: i64,
    ended: i64,
) -> String {
    store
        .upsert_session(&session(user, media, device, started))
        .unwrap();
    store
        .close_session(user, device, ended)
        .unwrap()
        .unwrap()
        .id
}

/// test-only：单事务原始 SQL 批量造历史（断言仍走公开 seam）。
fn raw_logs(data_dir: &Path, user: UserId, media: MediaId, ended_at: &[i64]) {
    let mut conn = rusqlite::Connection::open(data_dir.join("subscribe.db")).unwrap();
    let tx = conn.transaction().unwrap();
    {
        let mut stmt = tx
            .prepare(
                "INSERT INTO playback_logs
                    (id, user_id, media_id, season, episode, client, device_name,
                     play_method, started_at, ended_at, watched_ms, start_position_ms,
                     end_position_ms, duration_ms, completed, device_id)
                 VALUES (?1, ?2, ?3, NULL, NULL, NULL, NULL, 'local', ?4, ?5, 10, 0, 0, NULL, 0, NULL)",
            )
            .unwrap();
        for (index, ended) in ended_at.iter().enumerate() {
            stmt.execute(rusqlite::params![
                format!("raw-{index}"),
                user.to_string(),
                media.to_string(),
                ended - 1,
                ended,
            ])
            .unwrap();
        }
    }
    tx.commit().unwrap();
}

/// test-only 只读计数：store 没有公开的 `playback_metrics` 读取接口。
fn metric_count(data_dir: &Path, user: UserId) -> i64 {
    let conn = rusqlite::Connection::open(data_dir.join("subscribe.db")).unwrap();
    conn.query_row(
        "SELECT COUNT(*) FROM playback_metrics WHERE user_id = ?1",
        [user.to_string()],
        |row| row.get(0),
    )
    .unwrap()
}

fn log_ids(store: &Store, user: UserId) -> Vec<String> {
    store
        .list_logs(100_000, None, None, None, 0, Some(user))
        .unwrap()
        .into_iter()
        .map(|log| log.id)
        .collect()
}

fn scope(user: UserId) -> PlaybackLogScope {
    PlaybackLogScope {
        before: None,
        before_id: None,
        since: None,
        user_id: Some(user),
        visible_media: None,
    }
}

#[test]
fn deletes_only_logs_ended_strictly_before_cutoff() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let user = UserId::new();
    let media = MediaId::new();
    let cutoff = 1_700_000_000i64;
    let expired = log_fixture(&store, user, media, "expired", cutoff - 100, cutoff - 1);
    let on_cutoff = log_fixture(&store, user, media, "on-cutoff", cutoff - 100, cutoff);
    let fresh = log_fixture(&store, user, media, "fresh", cutoff - 100, cutoff + 1);

    let batch = store.prune_playback_logs(cutoff, 500, false).unwrap();

    assert_eq!(batch.candidates, 1);
    assert_eq!(batch.deleted, 1);
    assert!(!batch.has_more);
    // playback_logs 没有 cutoff 之外的额外保护规则。
    assert_eq!(batch.protected, 0);
    let kept = log_ids(&store, user);
    assert_eq!(kept.len(), 2);
    assert!(kept.contains(&on_cutoff), "ended_at == cutoff 必须保留");
    assert!(kept.contains(&fresh));
    assert!(!kept.contains(&expired));
}

#[test]
fn repeated_prune_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let user = UserId::new();
    let media = MediaId::new();
    let cutoff = 1_700_000_000i64;
    for index in 0..3 {
        log_fixture(&store, user, media, &format!("old{index}"), cutoff - 50, cutoff - 10 - index);
    }

    let first = store.prune_playback_logs(cutoff, 500, false).unwrap();
    assert_eq!(first.deleted, 3);
    let second = store.prune_playback_logs(cutoff, 500, false).unwrap();

    assert_eq!(second.candidates, 0);
    assert_eq!(second.deleted, 0);
    assert!(!second.has_more);
    assert!(log_ids(&store, user).is_empty());
}

#[test]
fn prune_keeps_progress_favorites_sessions_and_metrics() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let user = UserId::new();
    let media = MediaId::new();
    let cutoff = 1_700_000_000i64;
    store
        .upsert_unit(user, media, -1, -1, 42_000, Some(false), Some(true), Some(100_000), None, None, false, 100)
        .unwrap();
    let progress = store.unit_state(user, media, -1, -1).unwrap().unwrap();
    let live = session(user, media, "live", cutoff);
    store.upsert_session(&live).unwrap();
    store
        .insert_metric(user, media, 0, Some("web"), Some(120), 5, 1, 0, None, None, 1_000, cutoff)
        .unwrap();
    let metrics = metric_count(tmp.path(), user);
    for index in 0..2 {
        log_fixture(&store, user, media, &format!("old{index}"), cutoff - 50, cutoff - 10 - index);
    }

    let batch = store.prune_playback_logs(cutoff, 500, false).unwrap();

    assert_eq!(batch.deleted, 2);
    assert_eq!(store.unit_state(user, media, -1, -1).unwrap().unwrap(), progress);
    assert_eq!(store.get_session(user, "live").unwrap().unwrap(), live);
    let active = store.active_sessions(cutoff, 600).unwrap();
    assert_eq!(active.len(), 1, "活跃会话必须保留");
    assert_eq!(active[0].device_id, "live");
    assert_eq!(metric_count(tmp.path(), user), metrics);
    assert_eq!(metrics, 1);
}

#[test]
fn history_and_stats_only_cover_the_retention_window() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let user = UserId::new();
    let media = MediaId::new();
    let now = 1_700_000_000i64;
    let cutoff = now - 30 * DAY;
    let mut expired = Vec::new();
    for index in 0..3 {
        expired.push(log_fixture(
            &store,
            user,
            media,
            &format!("old{index}"),
            cutoff - 10 * DAY,
            cutoff - 5 * DAY - index,
        ));
    }
    let mut kept = Vec::new();
    for index in 0..2 {
        kept.push(log_fixture(
            &store,
            user,
            media,
            &format!("new{index}"),
            cutoff + DAY,
            cutoff + DAY + index,
        ));
    }
    assert_eq!(
        store.playback_watch_report(Some(user), None, now, 365, 0).unwrap().current.totals.plays,
        5
    );

    let batch = store.prune_playback_logs(cutoff, 500, false).unwrap();
    assert_eq!(batch.deleted, 3);

    let page = store.playback_log_page(&scope(user), 200).unwrap();
    assert_eq!(page.entries.len(), 2);
    assert!(!page.has_more);
    for entry in &page.entries {
        assert!(kept.contains(&entry.id));
        assert!(!expired.contains(&entry.id));
    }
    let report = store.playback_watch_report(Some(user), None, now, 365, 0).unwrap();
    assert_eq!(report.current.totals.plays, 2, "统计窗口内只剩保留数据");
    assert_eq!(report.current.totals.watched_ms, 20);
}

#[test]
fn dry_run_reports_candidates_without_writing() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let user = UserId::new();
    let media = MediaId::new();
    let cutoff = 1_700_000_000i64;
    for index in 0..3 {
        log_fixture(&store, user, media, &format!("old{index}"), cutoff - 50, cutoff - 10 - index);
    }
    let before = log_ids(&store, user);

    let preview = store.prune_playback_logs(cutoff, 500, true).unwrap();

    assert_eq!(preview.candidates, 3);
    assert_eq!(preview.deleted, 0);
    assert_eq!(log_ids(&store, user), before, "dry_run 必须零写入");

    let real = store.prune_playback_logs(cutoff, 500, false).unwrap();
    assert_eq!(real.deleted, 3);
    assert!(log_ids(&store, user).is_empty());
}

#[test]
fn batch_size_caps_each_run_and_reports_remaining_candidates() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let user = UserId::new();
    let media = MediaId::new();
    let cutoff = 1_700_000_000i64;
    let expired: Vec<i64> = (0..7).map(|index| cutoff - 1 - index).collect();
    raw_logs(tmp.path(), user, media, &expired);
    assert_eq!(log_ids(&store, user).len(), 7);

    let first = store.prune_playback_logs(cutoff, 3, false).unwrap();
    assert_eq!((first.candidates, first.deleted), (3, 3));
    assert!(first.has_more);
    let second = store.prune_playback_logs(cutoff, 3, false).unwrap();
    assert_eq!((second.candidates, second.deleted), (3, 3));
    assert!(second.has_more);
    let third = store.prune_playback_logs(cutoff, 3, false).unwrap();
    assert_eq!((third.candidates, third.deleted), (1, 1));
    assert!(!third.has_more);
    assert!(log_ids(&store, user).is_empty());
}

#[test]
fn delete_failure_rolls_back_the_whole_batch() {
    let tmp = tempfile::tempdir().unwrap();
    let user = UserId::new();
    let media = MediaId::new();
    let cutoff = 1_700_000_000i64;
    {
        let store = Store::open(tmp.path()).unwrap();
        for index in 0..3 {
            log_fixture(&store, user, media, &format!("old{index}"), cutoff - 50, cutoff - 1 - index);
        }
    }
    {
        let conn = rusqlite::Connection::open(tmp.path().join("subscribe.db")).unwrap();
        conn.execute_batch(
            "CREATE TRIGGER reject_playback_log_delete BEFORE DELETE ON playback_logs
             BEGIN SELECT RAISE(ABORT, 'injected delete failure'); END;",
        )
        .unwrap();
    }
    let store = Store::open(tmp.path()).unwrap();

    let result = store.prune_playback_logs(cutoff, 500, false);

    let error = result.expect_err("注入失败必须返回 Err");
    assert!(
        format!("{error}").contains("injected delete failure"),
        "错误必须来自注入的触发器，实际: {error}"
    );
    assert_eq!(log_ids(&store, user).len(), 3, "事务必须整体回滚");
}

#[test]
fn zero_batch_size_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();

    assert!(store.prune_playback_logs(1_700_000_000, 0, false).is_err());
}
