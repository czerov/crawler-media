//! T6 测试：Jobs 列表只抓取最新条目，即使旧行数据损坏或具有海量历史也不解码全量历史 (F07)。

use jobs::{JobKind, NewDef, Queue};

#[test]
fn corrupted_historical_job_payload_does_not_break_latest_child_query() {
    let tmp = tempfile::tempdir().unwrap();
    let queue = Queue::open(tmp.path().join("jobs.db")).unwrap();

    let def_id = queue
        .upsert_def(NewDef {
            kind: JobKind::Scrape,
            name: "test_def".into(),
            enabled: true,
            schedule: None,
            payload: "{}".into(),
            timeout_secs: None,
            concurrency_key: None,
        })
        .unwrap()
        .id;

    // 插入两条历史：一条是陈旧的（finished_at = 100），但是其 payload / progress 损坏；
    // 另一条是最新终态（finished_at = 200），数据完好。
    {
        let conn = rusqlite::Connection::open(tmp.path().join("jobs.db")).unwrap();
        // 插入两条历史：一条是陈旧的（finished_at = 100），但是其 payload / progress 损坏；
        // 另一条是最新终态（finished_at = 200），数据完好。
        let old_id = domain::JobId::new().to_string();
        let latest_id = domain::JobId::new().to_string();

        conn.execute(
            "INSERT INTO jobs (id, def_id, kind, payload, status, attempt, run_after, started_at, finished_at)
             VALUES (?1, ?2, 'invalid_kind_corrupted', 'bad', 'succeeded', 0, 100, 100, 100)",
            rusqlite::params![old_id, def_id.to_string()],
        )
        .unwrap();

        // 最新的有效终态
        conn.execute(
            "INSERT INTO jobs (id, def_id, kind, payload, status, attempt, run_after, started_at, finished_at)
             VALUES (?1, ?2, 'scrape', '{}', 'succeeded', 0, 200, 200, 200)",
            rusqlite::params![latest_id, def_id.to_string()],
        )
        .unwrap();
    }

    // children_for_defs 在使用窗口函数 / 关联子查询仅获取每 def 最新 1 条时，
    // 根本不会触碰、也不会反序列化陈旧的损坏行！
    let children = queue.children_for_defs(&[def_id]).unwrap();
    assert_eq!(children.len(), 1);
    let last = children[0].last.as_ref().expect("必须找到最新终态行");
    assert_eq!(last.status, jobs::JobStatus::Succeeded);
    assert_eq!(last.finished_at, Some(200));
}
