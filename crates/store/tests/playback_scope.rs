//! Store-side playback history scope: SQL keyset paging with a visible-Media
//! set and a hidden-row count for the same scope.

use domain::{MediaId, UserId};
use store::{PlaybackLogScope, PlayLogRow, SessionRow, Store};

/// 没有公开 insert_log：用 upsert_session + close_session 造一条 playback_logs。
fn log_fixture(
    store: &Store,
    user: UserId,
    media: MediaId,
    device: &str,
    started: i64,
    watched: i64,
) -> PlayLogRow {
    store
        .upsert_session(&SessionRow {
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
            watched_ms: watched,
            rate_bps: 0,
            bytes_sent: 0,
            connections: 0,
            admin_ended: false,
            started_at: started,
            last_report_at: started,
            ledger_id: None,
        })
        .unwrap();
    store.close_session(user, device, started + 1).unwrap().unwrap()
}

fn expected_order(mut rows: Vec<PlayLogRow>) -> Vec<PlayLogRow> {
    rows.sort_by(|a, b| {
        b.started_at
            .cmp(&a.started_at)
            .then_with(|| b.id.cmp(&a.id))
    });
    rows
}

fn ids(rows: &[PlayLogRow]) -> Vec<String> {
    rows.iter().map(|row| row.id.clone()).collect()
}

/// 按复合游标逐页翻完，返回所有可见行。
fn page_all(store: &Store, scope: &PlaybackLogScope, limit: usize) -> Vec<PlayLogRow> {
    let mut out = Vec::new();
    let mut cursor = (scope.before, scope.before_id.clone());
    loop {
        let mut scoped = scope.clone();
        scoped.before = cursor.0;
        scoped.before_id = cursor.1.clone();
        let page = store.playback_log_page(&scoped, limit).unwrap();
        let last = page.entries.last().cloned();
        out.extend(page.entries);
        if !page.has_more {
            break;
        }
        let last = last.expect("has_more implies a non-empty page");
        cursor = (Some(last.started_at), Some(last.id));
    }
    out
}

fn scope(
    user: UserId,
    visible: Option<Vec<MediaId>>,
    since: Option<i64>,
) -> PlaybackLogScope {
    PlaybackLogScope {
        before: None,
        before_id: None,
        since,
        user_id: Some(user),
        visible_media: visible,
    }
}

#[test]
fn composite_cursor_pages_visible_rows_without_gaps_or_duplicates() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let user = UserId::new();
    let visible = MediaId::new();
    let hidden = MediaId::new();
    let base = 1_700_000_000i64;
    let mut created = Vec::new();
    // 三组：每组一条可见、一条不可见，且组内 started_at 完全相同。
    for (index, started) in [base + 100, base + 50, base].into_iter().enumerate() {
        created.push(log_fixture(&store, user, visible, &format!("v{index}"), started, 10));
        created.push(log_fixture(&store, user, hidden, &format!("h{index}"), started, 10));
    }
    let expected = expected_order(
        created
            .into_iter()
            .filter(|row| row.media_id == visible)
            .collect(),
    );
    let scoped = scope(user, Some(vec![visible]), None);

    // limit=1 强制在 started_at 相同的行上走 id 次序，limit=2 走常规翻页。
    for limit in [1usize, 2, 3, 200] {
        let paged = page_all(&store, &scoped, limit);
        assert_eq!(ids(&paged), ids(&expected), "limit={limit}");
    }
    assert_eq!(expected.len(), 3);

    let first = store.playback_log_page(&scoped, 2).unwrap();
    assert_eq!(first.entries.len(), 2);
    assert!(first.has_more);
    assert_eq!(first.hidden_count, 3, "hidden rows must count for the same scope");
}

#[test]
fn hidden_count_counts_rows_not_distinct_media() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let user = UserId::new();
    let visible = MediaId::new();
    let hidden = MediaId::new();
    let base = 1_700_000_000i64;
    log_fixture(&store, user, visible, "v0", base, 10);
    // 同一不可见 Media 的两行：hidden_count 应为 2（行数），不是 1（distinct）。
    log_fixture(&store, user, hidden, "h0", base, 10);
    log_fixture(&store, user, hidden, "h1", base - 10, 10);

    let page = store
        .playback_log_page(&scope(user, Some(vec![visible]), None), 10)
        .unwrap();
    assert_eq!(page.entries.len(), 1);
    assert!(!page.has_more);
    assert_eq!(page.hidden_count, 2);
}

#[test]
fn empty_visible_scope_returns_empty_page() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let user = UserId::new();
    let first = MediaId::new();
    let second = MediaId::new();
    let base = 1_700_000_000i64;
    log_fixture(&store, user, first, "a", base, 10);
    log_fixture(&store, user, second, "b", base - 10, 10);

    let page = store
        .playback_log_page(&scope(user, Some(Vec::new()), None), 10)
        .unwrap();
    assert!(page.entries.is_empty());
    assert!(!page.has_more);
    assert_eq!(page.hidden_count, 2);
}

#[test]
fn none_visible_scope_is_unrestricted() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let user = UserId::new();
    let first = MediaId::new();
    let second = MediaId::new();
    let base = 1_700_000_000i64;
    log_fixture(&store, user, first, "a", base, 10);
    log_fixture(&store, user, second, "b", base - 10, 10);

    let page = store.playback_log_page(&scope(user, None, None), 10).unwrap();
    assert_eq!(page.entries.len(), 2);
    assert!(!page.has_more);
    assert_eq!(page.hidden_count, 0, "unrestricted scope hides nothing");
}

#[test]
fn since_filters_both_page_and_hidden_count() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let user = UserId::new();
    let visible = MediaId::new();
    let hidden = MediaId::new();
    let base = 1_700_000_000i64;
    log_fixture(&store, user, visible, "v-new", base, 10);
    log_fixture(&store, user, hidden, "h-new", base - 10, 10);
    log_fixture(&store, user, visible, "v-old", base - 100, 10);
    log_fixture(&store, user, hidden, "h-old", base - 200, 10);

    let page = store
        .playback_log_page(&scope(user, Some(vec![visible]), Some(base - 50)), 10)
        .unwrap();
    assert_eq!(ids(&page.entries), ids(&[log_id(&store, "v-new")]));
    assert_eq!(page.hidden_count, 1, "hidden count must honor since");
    assert!(!page.has_more);
}

/// 取某 device 的日志 id（测试内部用，避免依赖 UUID 顺序）。
fn log_id(store: &Store, device: &str) -> PlayLogRow {
    store
        .list_logs(10_000, None, None, None, 0, None)
        .unwrap()
        .into_iter()
        .find(|row| row.device_id.as_deref() == Some(device))
        .expect("log row for device")
}

#[test]
fn limit_bounds_and_ambiguous_cursor_are_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let user = UserId::new();
    let visible = MediaId::new();
    log_fixture(&store, user, visible, "v", 1_700_000_000, 10);
    let scoped = scope(user, Some(vec![visible]), None);

    assert!(store.playback_log_page(&scoped, 0).is_err());
    assert!(store.playback_log_page(&scoped, 201).is_err());
    assert!(store.playback_log_page(&scoped, 1).is_ok());
    assert!(store.playback_log_page(&scoped, 200).is_ok());

    let mut ambiguous = scoped.clone();
    ambiguous.before_id = Some("any-id".into());
    assert!(
        store.playback_log_page(&ambiguous, 10).is_err(),
        "before_id without before must be rejected"
    );

    let mut timestamp_only = scoped.clone();
    timestamp_only.before = Some(1_700_000_000);
    assert!(store.playback_log_page(&timestamp_only, 10).is_ok());
}

#[test]
fn scope_does_not_leak_between_calls() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let user = UserId::new();
    let first = MediaId::new();
    let second = MediaId::new();
    let base = 1_700_000_000i64;
    log_fixture(&store, user, first, "a", base, 10);
    log_fixture(&store, user, second, "b", base - 10, 10);

    // 第一次填满大可见集合。
    let filled = store
        .playback_log_page(&scope(user, Some(vec![first]), None), 10)
        .unwrap();
    assert_eq!(filled.entries.len(), 1);
    assert_eq!(filled.hidden_count, 1);

    // 第二次空可见集合：若上一次的 scope 行残留，hidden_count 会偏小。
    let empty = store
        .playback_log_page(&scope(user, Some(Vec::new()), None), 10)
        .unwrap();
    assert!(empty.entries.is_empty());
    assert!(!empty.has_more);
    assert_eq!(
        empty.hidden_count, 2,
        "previous scope rows must not survive into this call"
    );

    // 第三次换成另一个可见集合，仍只看到它自己的行。
    let swapped = store
        .playback_log_page(&scope(user, Some(vec![second]), None), 10)
        .unwrap();
    assert_eq!(ids(&swapped.entries), ids(&[log_id(&store, "b")]));
    assert_eq!(swapped.hidden_count, 1);
}

#[test]
fn user_scope_filters_page_and_hidden() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let alice = UserId::new();
    let bob = UserId::new();
    let visible = MediaId::new();
    let hidden = MediaId::new();
    let base = 1_700_000_000i64;
    log_fixture(&store, alice, visible, "alice-v", base, 10);
    log_fixture(&store, alice, hidden, "alice-h", base - 10, 10);
    log_fixture(&store, bob, visible, "bob-v", base - 20, 10);

    let page = store
        .playback_log_page(&scope(alice, Some(vec![visible]), None), 10)
        .unwrap();
    assert_eq!(ids(&page.entries), ids(&[log_id(&store, "alice-v")]));
    assert_eq!(page.hidden_count, 1, "bob's rows must not count for alice");
}
