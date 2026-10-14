use jobs::{JobKind, NewDef, Queue};

/// B3 §5.1：窗口未满即 cycle 结束并清游标；旧实现只在**下一个空窗口**才结束，
/// 每个 cycle 都要多空跑一次，而且游标会一直挂着。
#[test]
fn retention_cycle_completes_on_the_last_partial_window() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("jobs.db");
    drop(Queue::open(&db).unwrap());

    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        for i in 0..3i64 {
            conn.execute(
                "INSERT INTO jobs (id, def_id, kind, payload, status, attempt, run_after, started_at, finished_at)
                 VALUES (?1, NULL, 'scrape', '{}', 'succeeded', 0, ?2, ?2, ?2)",
                rusqlite::params![format!("deletable-{i}"), 100 + i],
            )
            .unwrap();
        }
    }

    let queue = Queue::open(&db).unwrap();
    let win = queue
        .prune_terminal_window("test-def", 1_000_000, 500, false)
        .unwrap();
    assert_eq!(win.deleted, 3);
    assert!(!win.window_has_more);
    assert!(
        win.cycle_complete,
        "窗口未满就说明没有更多候选，cycle 到此结束: {win:?}"
    );

    let cursors: i64 = rusqlite::Connection::open(&db)
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM retention_cursors WHERE def_id = 'test-def'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(cursors, 0, "cycle 结束后必须清掉游标，不留空跑收尾");
}

#[test]
fn retention_cursor_window_bounds_and_cleans_older_jobs() {
    let tmp = tempfile::tempdir().unwrap();
    let queue = Queue::open(tmp.path().join("jobs.db")).unwrap();

    let def_id = queue
        .upsert_def(NewDef {
            kind: JobKind::CatalogRefresh,
            name: "retention_test".into(),
            enabled: true,
            schedule: None,
            payload: "{}".into(),
            timeout_secs: None,
            concurrency_key: None,
        })
        .unwrap()
        .id;

    // Insert 5 jobs with distinct finished_at
    for i in 1..=5 {
        let job = queue.ensure_scheduled_for(0, def_id).unwrap().remove(0);
        let claimed = queue.claim(0, 1).unwrap().remove(0);
        assert_eq!(claimed.id, job.id);
        queue.succeed(job.id, i).unwrap();
    }

    // Now prune with limit 2 at cutoff in the future
    let cutoff = 1_000_000;
    let win1 = queue.prune_terminal_window("test-def", cutoff, 2, false).unwrap();
    assert_eq!(win1.scanned, 2);
    assert_eq!(win1.deleted, 2);
    assert!(win1.window_has_more);

    // Second window
    let win2 = queue.prune_terminal_window("test-def", cutoff, 2, false).unwrap();
    assert_eq!(win2.scanned, 2);
    assert_eq!(win2.deleted, 2);

    // Third window: only 1 job left (the latest one!), which is protected by latest child rule
    let win3 = queue.prune_terminal_window("test-def", cutoff, 2, false).unwrap();
    assert_eq!(win3.scanned, 1);
    assert_eq!(win3.protected, 1);
    assert_eq!(win3.deleted, 0);
}

/// 验收门槛（B3 §7）：连续多个"整窗都是受保护行"的窗口之后，游标仍必须推进到
/// 后面的可删历史，不能被 protected 前缀饿死。
#[test]
fn retention_cursor_is_not_starved_by_a_protected_prefix() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("jobs.db");
    drop(Queue::open(&db).unwrap());

    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        // 20 个受保护行：各自属于独立 def，且是该 def 唯一的终态子任务。
        for i in 0..20i64 {
            conn.execute(
                "INSERT INTO jobs (id, def_id, kind, payload, status, attempt, run_after, started_at, finished_at)
                 VALUES (?1, ?2, 'scrape', '{}', 'succeeded', 0, ?3, ?3, ?3)",
                rusqlite::params![format!("protected-{i}"), format!("def-{i}"), 100 + i],
            )
            .unwrap();
        }
        // 5 个可删行（def_id 为 NULL），finished_at 更晚。
        for i in 0..5i64 {
            conn.execute(
                "INSERT INTO jobs (id, def_id, kind, payload, status, attempt, run_after, started_at, finished_at)
                 VALUES (?1, NULL, 'scrape', '{}', 'succeeded', 0, ?2, ?2, ?2)",
                rusqlite::params![format!("deletable-{i}"), 1000 + i],
            )
            .unwrap();
        }
    }

    let queue = Queue::open(&db).unwrap();
    let cutoff = 1_000_000;
    let mut deleted = 0usize;
    let mut protected = 0usize;
    let mut windows = 0usize;
    loop {
        let win = queue.prune_terminal_window("test-def", cutoff, 2, false).unwrap();
        deleted += win.deleted;
        protected += win.protected;
        windows += 1;
        if !win.window_has_more {
            break;
        }
        assert!(windows < 40, "游标没有推进：窗口数 {}", windows);
    }

    assert_eq!(protected, 20, "保护行也必须被检视并计数");
    assert_eq!(
        deleted, 5,
        "受保护前缀之后的 5 条可删历史必须被清理（{} 个窗口）",
        windows
    );
}
