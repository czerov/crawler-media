//! B6：SQL 聚合的播放统计——去重、窗口边界、时区桶与零填充。

use domain::{MediaId, UserId};
use store::{SessionRow, Store, WatchReport};

const DAY: i64 = 86_400;

fn log_fixture(
    store: &Store,
    user: UserId,
    media: MediaId,
    device: &str,
    started: i64,
    watched: i64,
) {
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
    store.close_session(user, device, started + 1).unwrap().unwrap();
}

fn day_plays(report: &WatchReport, day: i64) -> i64 {
    report
        .current
        .days
        .iter()
        .find(|entry| entry.day == day)
        .map(|entry| entry.totals.plays)
        .unwrap_or(-1)
}

fn day_watched(report: &WatchReport, day: i64) -> i64 {
    report
        .current
        .days
        .iter()
        .find(|entry| entry.day == day)
        .map(|entry| entry.totals.watched_ms)
        .unwrap_or(-1)
}

#[test]
fn dedupes_members_and_media_and_zero_fills() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let user = UserId::new();
    let media = MediaId::new();
    let now = 1_700_000_000i64;
    for (index, watched) in [1_000i64, 2_000, 3_000].into_iter().enumerate() {
        log_fixture(&store, user, media, &format!("d{index}"), now - index as i64 * 60, watched);
    }

    let report = store
        .playback_watch_report(None, None, now, 7, 0)
        .unwrap();

    assert_eq!(report.current.totals.plays, 3);
    assert_eq!(report.current.totals.watched_ms, 6_000);
    assert_eq!(report.current.totals.members, 1, "同一成员的多条日志只算一次");
    assert_eq!(report.current.media.len(), 1, "同一 Media 只出现一行");
    assert_eq!(report.current.media[0].plays, 3);
    assert_eq!(report.current.media[0].members, 1);
    assert_eq!(report.current.users.len(), 1);
    assert_eq!(report.current.users[0].user_id, user);
    assert_eq!(report.current.users[0].totals.plays, 3);

    // 零填充：days 行、7×24。
    assert_eq!(report.current.days.len(), 7);
    assert_eq!(report.current.hours.len(), 7);
    assert!(report.current.hours.iter().all(|row| row.len() == 24));
    assert_eq!(report.current.days.iter().map(|d| d.totals.plays).sum::<i64>(), 3);

    assert_eq!(report.previous.totals.plays, 0, "上一窗口为空");
    assert_eq!(report.previous.days.len(), 7);
    assert_eq!(report.hidden_titles, 0, "visible=None 不隐藏任何 Media");
}

#[test]
fn hidden_titles_counts_distinct_invisible_media_in_the_current_window() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let user = UserId::new();
    let visible = MediaId::new();
    let hidden = MediaId::new();
    let now = 1_700_000_000i64;
    log_fixture(&store, user, visible, "v0", now - 10, 1_000);
    // 同一不可见 Media 三条 → hidden_titles 仍是 1（distinct），但计数是行数。
    log_fixture(&store, user, hidden, "h0", now - 20, 500);
    log_fixture(&store, user, hidden, "h1", now - 30, 500);
    log_fixture(&store, user, hidden, "h2", now - 40, 500);
    // 上一窗口的不可见日志不计入 hidden_titles。
    log_fixture(&store, user, hidden, "h-old", now - 10 * DAY, 500);

    let scope = [visible];
    let report = store
        .playback_watch_report(None, Some(&scope), now, 7, 0)
        .unwrap();

    assert_eq!(report.hidden_titles, 1, "distinct Media，不是行数");
    assert_eq!(report.current.totals.plays, 1, "不可见 Media 不进 totals");
    assert_eq!(report.current.media.len(), 1);
    assert_eq!(report.current.media[0].media_id, visible);
    assert_eq!(report.previous.totals.plays, 0, "上一窗口的不可见日志也不可见");

    // 空可见集合：全部隐藏，totals 为空但 hidden_titles 是当前窗口 distinct。
    let report = store
        .playback_watch_report(None, Some(&[]), now, 7, 0)
        .unwrap();
    assert_eq!(report.current.totals.plays, 0);
    assert!(report.current.media.is_empty());
    assert_eq!(report.hidden_titles, 2, "visible + hidden 两个 Media 都不可见");
}

#[test]
fn previous_window_boundaries_are_half_open() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let user = UserId::new();
    let media = MediaId::new();
    let now = 1_700_000_000i64;
    let days = 7;
    let boundary = now - days * DAY;
    // 边界（含）→ 当前窗口；边界前一秒 → 上一窗口。
    log_fixture(&store, user, media, "current-edge", boundary, 100);
    log_fixture(&store, user, media, "previous-edge", boundary - 1, 100);
    // 上一窗口起点（含）；再往前一秒两边都不算。
    log_fixture(&store, user, media, "previous-start", now - 2 * days * DAY, 100);
    log_fixture(&store, user, media, "outside", now - 2 * days * DAY - 1, 100);

    let report = store
        .playback_watch_report(None, None, now, days as u16, 0)
        .unwrap();
    assert_eq!(report.current.totals.plays, 1, "当前窗口 [now-days, ∞)");
    assert_eq!(report.previous.totals.plays, 2, "上一窗口 [now-2*days, now-days)");
}

#[test]
fn timezone_buckets_match_euclid_semantics() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let user = UserId::new();
    let media = MediaId::new();
    let now = DAY;
    // 负 epoch 边界：-1 / 0 / 86399 / 86400。
    for (index, (started, watched)) in
        [(-1i64, 1_000i64), (0, 2_000), (86_399, 3_000), (86_400, 4_000)]
            .into_iter()
            .enumerate()
    {
        log_fixture(&store, user, media, &format!("tz{index}"), started, watched);
    }

    // tz = 0：day 用 floor 除法，-1 属于 day -1；hour 用 rem_euclid，-1 → 23 点。
    let report = store.playback_watch_report(None, None, now, 3, 0).unwrap();
    assert_eq!(day_plays(&report, -1), 1);
    assert_eq!(day_plays(&report, 0), 2);
    assert_eq!(day_plays(&report, 1), 1);
    assert_eq!(report.current.hours[2][23], 1_000, "weekday(-1)=2");
    assert_eq!(report.current.hours[3][0], 2_000);
    assert_eq!(report.current.hours[3][23], 3_000);
    assert_eq!(report.current.hours[4][0], 4_000, "weekday(1)=4");

    // tz = +480（+8h）：today=1，区间 [-1,1]；day0 收 -1/0，day1 收 86399/86400。
    let report = store.playback_watch_report(None, None, now, 3, 480).unwrap();
    assert_eq!(day_plays(&report, -1), 0);
    assert_eq!(day_plays(&report, 0), 2);
    assert_eq!(day_plays(&report, 1), 2);
    assert_eq!(report.current.hours[3][7], 1_000);
    assert_eq!(report.current.hours[3][8], 2_000);
    assert_eq!(report.current.hours[4][7], 3_000);
    assert_eq!(report.current.hours[4][8], 4_000);

    // tz = -480（-8h）：today=0，区间 [-2,0]；负 epoch 的 floor 除法是关键。
    let report = store.playback_watch_report(None, None, now, 3, -480).unwrap();
    assert_eq!(day_plays(&report, -2), 0);
    assert_eq!(day_plays(&report, -1), 2);
    assert_eq!(day_plays(&report, 0), 2);
    assert_eq!(day_watched(&report, -1), 3_000);
    assert_eq!(day_watched(&report, 0), 7_000);
    assert_eq!(report.current.hours[2][15], 1_000, "weekday(-1)=2");
    assert_eq!(report.current.hours[2][16], 2_000);
    assert_eq!(report.current.hours[3][15], 3_000);
    assert_eq!(report.current.hours[3][16], 4_000);
}

#[test]
fn validates_window_and_timezone_bounds() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let now = 1_700_000_000i64;

    assert!(store.playback_watch_report(None, None, now, 0, 0).is_err());
    assert!(store.playback_watch_report(None, None, now, 366, 0).is_err());
    assert!(store.playback_watch_report(None, None, now, 1, 1_441).is_err());
    assert!(store.playback_watch_report(None, None, now, 1, -1_441).is_err());
    assert!(store.playback_watch_report(None, None, now, 1, 1_440).is_ok());
    assert!(store.playback_watch_report(None, None, now, 365, -1_440).is_ok());
}

#[test]
fn scope_rows_do_not_leak_between_calls() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let user = UserId::new();
    let first = MediaId::new();
    let second = MediaId::new();
    let now = 1_700_000_000i64;
    log_fixture(&store, user, first, "a", now - 10, 1_000);
    log_fixture(&store, user, second, "b", now - 20, 1_000);

    let scoped = [first];
    let report = store
        .playback_watch_report(None, Some(&scoped), now, 7, 0)
        .unwrap();
    assert_eq!(report.current.media[0].media_id, first);
    assert_eq!(report.hidden_titles, 1);

    // 第二次换成空集合：若上一次的 scope 行残留，hidden_titles 会偏小。
    let report = store
        .playback_watch_report(None, Some(&[]), now, 7, 0)
        .unwrap();
    assert!(report.current.media.is_empty());
    assert_eq!(report.hidden_titles, 2, "上一次的可见集合不得残留");

    // 第三次换成另一个集合。
    let scoped = [second];
    let report = store
        .playback_watch_report(None, Some(&scoped), now, 7, 0)
        .unwrap();
    assert_eq!(report.current.media[0].media_id, second);
    assert_eq!(report.hidden_titles, 1);
}
