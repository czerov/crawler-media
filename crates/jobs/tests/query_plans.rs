//! DB5：jobs 热查询的索引证据与查询形状回归。
//!
//! 默认只跑正确性回归。索引/耗时证据（20 万行合成数据、ANALYZE、EQP、中位数耗时）
//! 用下面的命令重新生成，输出可直接贴进
//! `docs/audits/2026-10-12-remediation-review-repro/jobs-query-plans.md`：
//!
//! ```text
//! cargo test -p jobs --test query_plans -- --ignored --nocapture
//! ```

use std::path::Path;
use std::time::Instant;

use jobs::{JobKind, NewDef, Queue, Schedule};
use rusqlite::Connection;

/// 与 `store.rs::key_busy` 逐字相同的查询。
const KEY_BUSY_SQL: &str = "SELECT COUNT(*) FROM jobs
     WHERE status = 'running' AND id != ?3 AND (
         (?1 IS NOT NULL AND concurrency_key = ?1) OR
         (?2 IS NOT NULL AND def_id = ?2)
     )";

/// `key_busy` 拆成两次探测（concurrency_key / def_id 各一次）。
const KEY_BUSY_BY_KEY_SQL: &str = "SELECT EXISTS(SELECT 1 FROM jobs
     WHERE status = 'running' AND id != ?1 AND concurrency_key = ?2)";
const KEY_BUSY_BY_DEF_SQL: &str = "SELECT EXISTS(SELECT 1 FROM jobs
     WHERE status = 'running' AND id != ?1 AND def_id = ?2)";

const LIVE_CHILD_SQL: &str = "SELECT id, def_id, kind, payload, status, attempt, run_after, \
     started_at, finished_at, error, progress, concurrency_key, timeout_secs FROM jobs
     WHERE def_id = ?1 AND status IN ('queued','running')
     ORDER BY CASE status WHEN 'queued' THEN 0 ELSE 1 END, run_after DESC LIMIT 1";

const LAST_CHILD_SQL: &str = "SELECT id, def_id, kind, payload, status, attempt, run_after, \
     started_at, finished_at, error, progress, concurrency_key, timeout_secs FROM jobs
     WHERE def_id = ?1 AND status IN ('succeeded','failed','cancelled')
     ORDER BY finished_at DESC, run_after DESC LIMIT 1";

const LIST_RUNNING_SQL: &str =
    "SELECT id, def_id, kind, payload, status, attempt, run_after, started_at, finished_at, \
     error, progress, concurrency_key, timeout_secs FROM jobs WHERE status = 'running'";

/// 候选索引：只保留有实测收益的（见归档文档）。
const CANDIDATE_INDEXES: &[(&str, &str)] = &[
    (
        "jobs_running_key_idx",
        "CREATE INDEX IF NOT EXISTS jobs_running_key_idx ON jobs(concurrency_key) \
         WHERE status = 'running'",
    ),
    (
        "jobs_live_def_idx",
        "CREATE INDEX IF NOT EXISTS jobs_live_def_idx ON jobs(def_id, status, run_after) \
         WHERE status IN ('queued','running')",
    ),
    (
        "jobs_last_def_idx",
        "CREATE INDEX IF NOT EXISTS jobs_last_def_idx \
         ON jobs(def_id, finished_at DESC, run_after DESC) \
         WHERE status IN ('succeeded','failed','cancelled')",
    ),
    (
        "jobs_running_status_idx",
        "CREATE INDEX IF NOT EXISTS jobs_running_status_idx ON jobs(status) \
         WHERE status = 'running'",
    ),
];

fn open_raw(path: &Path) -> Connection {
    let conn = Connection::open(path).unwrap();
    conn.pragma_update(None, "journal_mode", "WAL").unwrap();
    conn.busy_timeout(std::time::Duration::from_secs(10)).unwrap();
    conn
}

/// 20 万行合成数据：200 running / 1800 queued / 其余终止态，
/// 500 个 def 承载终止态子任务，running/queued 各自挂到独立 def。
fn seed_dataset(conn: &Connection, jobs: i64) {
    let tx = conn.unchecked_transaction().unwrap();
    {
        let mut stmt = tx
            .prepare(
                "INSERT INTO jobs (id, def_id, kind, payload, status, attempt, run_after,
                    started_at, finished_at, error, progress, concurrency_key, timeout_secs)
                 VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6, ?7, ?8, NULL, NULL, ?9, NULL)",
            )
            .unwrap();
        // JOBS_DENSE_RUNNING=1 是敏感性检查：把 running 从 200 提到 10_000，
        // 观察 OR 形状随 running 集合增大的退化（见归档文档）。
        let stride = if std::env::var("JOBS_DENSE_RUNNING").is_ok() { 20 } else { 1000 };
        for i in 0..jobs {
            let running = i % stride == 0;
            let queued = !running && i % 100 == 0;
            let (def_id, status, started, finished, key) = if running {
                (
                    format!("def-run-{i}"),
                    "running",
                    Some(1_700_000_000 + i),
                    None,
                    Some(format!("sub-{}", (i / stride) % 50)),
                )
            } else if queued {
                (
                    format!("def-queued-{}", i / 100),
                    "queued",
                    None,
                    None,
                    Some(format!("sub-{}", i % 50)),
                )
            } else {
                let status = match i % 3 {
                    0 => "succeeded",
                    1 => "failed",
                    _ => "cancelled",
                };
                (
                    format!("def-{}", i % 500),
                    status,
                    Some(1_700_000_000 + i),
                    Some(1_700_000_000 + (i % 10_000)),
                    None,
                )
            };
            stmt.execute(rusqlite::params![
                format!("job-{i:07}"),
                def_id,
                "catalog_refresh",
                format!("p{}", i % 20),
                status,
                1_700_000_000 + (i % 1000),
                started,
                finished,
                key,
            ])
            .unwrap();
        }
    }
    tx.commit().unwrap();
}

fn eqp(conn: &Connection, sql: &str, args: Vec<rusqlite::types::Value>) -> Vec<String> {
    let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
    let rows = stmt
        .query_map(rusqlite::params_from_iter(args), |row| {
            row.get::<_, String>(3)
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    rows
}

fn text(value: &str) -> rusqlite::types::Value {
    rusqlite::types::Value::Text(value.to_string())
}

fn plan_args(name: &str) -> Vec<rusqlite::types::Value> {
    match name {
        "key_busy (OR, rejected alternative)" => {
            vec![text("sub-7"), text("def-run-1000"), text("job-0000000")]
        }
        "key_busy split probe: concurrency_key" => {
            vec![text("job-0000000"), text("sub-7")]
        }
        "key_busy split probe: def_id" => vec![text("job-0000000"), text("def-run-1000")],
        "get_live_child" => vec![text("def-mixed")],
        "get_last_child" => vec![text("def-7")],
        _ => Vec::new(),
    }
}

/// 返回 (中位数, 最小值) 毫秒。共享机器上并行负载会拉高中位数，最小值更能反映
/// 查询本身的开销；两者都记录，不据此外推生产。
fn sample_ms(runs: usize, mut run: impl FnMut()) -> (f64, f64) {
    let mut samples = Vec::new();
    for _ in 0..runs {
        let start = Instant::now();
        run();
        samples.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    (samples[samples.len() / 2], samples[0])
}

fn sqlite_version(conn: &Connection) -> String {
    conn.query_row("SELECT sqlite_version()", [], |row| row.get(0))
        .unwrap()
}

/// key_busy 的 hit / miss 两种情形分别计时：EXISTS 在没有索引时会在第一个匹配行
/// 提前停止，所以 miss 才是退化成全表扫描的最坏情形。
fn time_key_busy(
    conn: &Connection,
    or_label: &'static str,
    split_label: &'static str,
    key: &str,
    def: &str,
    except: &str,
) -> Vec<(&'static str, (f64, f64))> {
    let mut busy = conn.prepare(KEY_BUSY_SQL).unwrap();
    let or_ms = sample_ms(51, || {
        let count: i64 = busy
            .query_row(rusqlite::params![key, def, except], |row| row.get(0))
            .unwrap();
        std::hint::black_box(count);
    });
    let mut by_key = conn.prepare(KEY_BUSY_BY_KEY_SQL).unwrap();
    let mut by_def = conn.prepare(KEY_BUSY_BY_DEF_SQL).unwrap();
    let split_ms = sample_ms(51, || {
        let a: bool = by_key
            .query_row(rusqlite::params![except, key], |row| row.get(0))
            .unwrap();
        let b: bool = by_def
            .query_row(rusqlite::params![except, def], |row| row.get(0))
            .unwrap();
        std::hint::black_box(a | b);
    });
    vec![(or_label, or_ms), (split_label, split_ms)]
}

fn time_queries(conn: &Connection) -> Vec<(&'static str, (f64, f64))> {
    let except = "job-0000000";
    let mut out = time_key_busy(
        conn,
        "key_busy (OR, rejected) hit",
        "key_busy split (shipped) hit",
        "sub-7",
        "def-run-1000",
        except,
    );
    out.extend(time_key_busy(
        conn,
        "key_busy (OR, rejected) miss",
        "key_busy split (shipped) miss",
        "sub-999",
        "def-run-999999",
        except,
    ));

    let mut live = conn.prepare(LIVE_CHILD_SQL).unwrap();
    out.push((
        "get_live_child",
        sample_ms(51, || {
            let hit: Option<String> = live
                .query_row(rusqlite::params!["def-mixed"], |row| row.get(0))
                .ok();
            std::hint::black_box(hit);
        }),
    ));

    let mut last = conn.prepare(LAST_CHILD_SQL).unwrap();
    out.push((
        "get_last_child",
        sample_ms(51, || {
            let hit: Option<String> = last
                .query_row(rusqlite::params!["def-7"], |row| row.get(0))
                .ok();
            std::hint::black_box(hit);
        }),
    ));

    let mut running = conn.prepare(LIST_RUNNING_SQL).unwrap();
    out.push((
        "list_running",
        sample_ms(51, || {
            let rows: Vec<String> = running
                .query_map([], |row| row.get(0))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            std::hint::black_box(rows.len());
        }),
    ));
    out
}

fn print_state(label: &str, conn: &Connection, timings: &[(&'static str, (f64, f64))]) {
    println!("\n===== {label} =====");
    for (name, sql) in [
        ("key_busy (OR, rejected alternative)", KEY_BUSY_SQL),
        ("key_busy split probe: concurrency_key", KEY_BUSY_BY_KEY_SQL),
        ("key_busy split probe: def_id", KEY_BUSY_BY_DEF_SQL),
        ("get_live_child", LIVE_CHILD_SQL),
        ("get_last_child", LAST_CHILD_SQL),
        ("list_running", LIST_RUNNING_SQL),
    ] {
        println!("--- {name}");
        for line in eqp(conn, sql, plan_args(name)) {
            println!("{line}");
        }
    }
    println!("--- ms over 51 runs (median / min)");
    for (name, (median, min)) in timings {
        println!("{name}\tmedian={median:.3}\tmin={min:.3}");
    }
}

fn drop_candidates(conn: &Connection) {
    for (name, _) in CANDIDATE_INDEXES {
        conn.execute_batch(&format!("DROP INDEX IF EXISTS {name};")).unwrap();
    }
}

fn create_candidates(conn: &Connection) {
    for (_, ddl) in CANDIDATE_INDEXES {
        conn.execute_batch(&format!("{ddl};")).unwrap();
    }
}

/// 生成归档用的 before/after 证据。
#[test]
#[ignore = "measurement harness; run with --ignored --nocapture to regenerate the audit evidence"]
fn measure_jobs_query_plans() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("jobs.db");
    let _queue = Queue::open(&path).unwrap();
    let conn = open_raw(&path);

    let started = Instant::now();
    seed_dataset(&conn, 200_000);
    // 同一 def 同时有 queued + running 的混合子任务（get_live_child 的排序场景）。
    conn.execute_batch(
        "INSERT INTO jobs (id, def_id, kind, payload, status, attempt, run_after, concurrency_key)
         VALUES ('job-mixed-queued', 'def-mixed', 'catalog_refresh', 'p0', 'queued', 0, 5, 'sub-1');
         INSERT INTO jobs (id, def_id, kind, payload, status, attempt, run_after, concurrency_key)
         VALUES ('job-mixed-running', 'def-mixed', 'catalog_refresh', 'p0', 'running', 0, 9, 'sub-2');",
    )
    .unwrap();
    println!("sqlite version: {}", sqlite_version(&conn));
    println!("seed: 200000 jobs in {:.2}s", started.elapsed().as_secs_f64());

    drop_candidates(&conn);
    conn.execute_batch("ANALYZE;").unwrap();
    let before = time_queries(&conn);
    print_state("BEFORE (baseline indexes only, ANALYZE)", &conn, &before);

    create_candidates(&conn);
    conn.execute_batch("ANALYZE;").unwrap();
    let after = time_queries(&conn);
    print_state("AFTER (shipped indexes + ANALYZE)", &conn, &after);

    println!("\n===== summary (ms, 51 runs) =====");
    println!("query\tbefore_median\tbefore_min\tafter_median\tafter_min");
    for ((name, (bm, bmin)), (_, (am, amin))) in before.iter().zip(after.iter()) {
        println!("{name}\t{bm:.3}\t{bmin:.3}\t{am:.3}\t{amin:.3}");
    }
}

// ---------------------------------------------------------------------------
// 正确性回归（默认运行）
// ---------------------------------------------------------------------------

fn def(name: &str) -> NewDef {
    NewDef {
        kind: JobKind::CatalogRefresh,
        name: name.into(),
        enabled: true,
        schedule: Some(Schedule::Interval { secs: 3_600 }),
        payload: name.into(),
        timeout_secs: None,
        concurrency_key: None,
    }
}

/// 直接写库造出精确状态；返回生成的 job id。
fn insert_job(
    conn: &Connection,
    def_id: Option<&str>,
    status: &str,
    run_after: i64,
    key: Option<&str>,
    finished_at: Option<i64>,
) -> String {
    let id = domain::JobId::new().to_string();
    conn.execute(
        "INSERT INTO jobs (id, def_id, kind, payload, status, attempt, run_after,
            started_at, finished_at, concurrency_key)
         VALUES (?1, ?2, 'catalog_refresh', 'p', ?3, 0, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            id,
            def_id,
            status,
            run_after,
            (status == "running").then_some(1_000i64),
            finished_at,
            key,
        ],
    )
    .unwrap();
    id
}

fn setup() -> (tempfile::TempDir, Queue, Connection) {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("jobs.db");
    let queue = Queue::open(&path).unwrap();
    let conn = open_raw(&path);
    (tmp, queue, conn)
}

#[test]
fn live_child_prefers_queued_over_running() {
    let (_tmp, queue, conn) = setup();
    let d = queue.upsert_def(def("live")).unwrap();
    let def_id = d.id.to_string();
    // running 的 run_after 更新，queued 更旧：状态优先级必须压过 run_after。
    insert_job(&conn, Some(&def_id), "running", 900, None, None);
    let queued = insert_job(&conn, Some(&def_id), "queued", 5, None, None);

    let live = queue.get_live_child(d.id).unwrap().expect("live child");
    assert_eq!(live.id.to_string(), queued);
    assert_eq!(live.status.as_str(), "queued");
}

#[test]
fn live_child_orders_same_status_by_run_after_desc() {
    let (_tmp, queue, conn) = setup();
    let d = queue.upsert_def(def("live-order")).unwrap();
    let def_id = d.id.to_string();
    // 生产约束保证同一 def 不会有两条同状态 live child（见 queued_child_is_unique_per_def），
    // 这里临时去掉 running 的部分唯一索引，单独验证 ORDER BY 的 run_after DESC 兜底键。
    conn.execute_batch("DROP INDEX jobs_running_def;").unwrap();
    insert_job(&conn, Some(&def_id), "running", 100, None, None);
    let newer = insert_job(&conn, Some(&def_id), "running", 200, None, None);

    let live = queue.get_live_child(d.id).unwrap().expect("live child");
    assert_eq!(live.id.to_string(), newer, "同状态取 run_after 更大者");
}

#[test]
fn last_child_orders_by_finished_at_then_run_after() {
    let (_tmp, queue, conn) = setup();
    let d = queue.upsert_def(def("last")).unwrap();
    let def_id = d.id.to_string();
    insert_job(&conn, Some(&def_id), "succeeded", 10, None, Some(500));
    insert_job(&conn, Some(&def_id), "failed", 10, None, Some(600));
    // finished_at 与上一条并列，run_after 更大者胜出。
    let newest = insert_job(&conn, Some(&def_id), "cancelled", 20, None, Some(600));

    let last = queue.get_last_child(d.id).unwrap().expect("last child");
    assert_eq!(last.id.to_string(), newest);
    assert_eq!(last.status.as_str(), "cancelled");
}

#[test]
fn key_busy_truth_table_via_claim() {
    // 1) concurrency_key 命中 → 忙，不领取。
    let (_tmp, queue, conn) = setup();
    insert_job(&conn, Some(&new_def_id()), "running", 0, Some("k1"), None);
    insert_job(&conn, None, "queued", 0, Some("k1"), None);
    assert!(queue.claim(100, 10).unwrap().is_empty(), "同 key 的 running 必须阻塞");

    // 2) def_id 命中 → 忙，不领取。
    let (_tmp, queue, conn) = setup();
    let shared_def = new_def_id();
    insert_job(&conn, Some(&shared_def), "running", 0, None, None);
    insert_job(&conn, Some(&shared_def), "queued", 0, None, None);
    assert!(queue.claim(100, 10).unwrap().is_empty(), "同 def 的 running 必须阻塞");

    // 3) key / def 都是 NULL → 不忙，正常领取。
    let (_tmp, queue, conn) = setup();
    insert_job(&conn, Some(&new_def_id()), "running", 0, Some("k2"), None);
    let free = insert_job(&conn, None, "queued", 0, None, None);
    let claimed = queue.claim(100, 10).unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].id.to_string(), free);

    // 4) key / def 都不匹配 → 不忙。
    let (_tmp, queue, conn) = setup();
    insert_job(&conn, Some(&new_def_id()), "running", 0, Some("k3"), None);
    let other = insert_job(&conn, Some(&new_def_id()), "queued", 0, Some("k4"), None);
    let claimed = queue.claim(100, 10).unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].id.to_string(), other);
}

fn new_def_id() -> String {
    domain::JobDefId::new().to_string()
}

#[test]
fn queued_child_is_unique_per_def() {
    let (_tmp, queue, conn) = setup();
    let d = queue.upsert_def(def("unique-queued")).unwrap();
    let def_id = d.id.to_string();
    insert_job(&conn, Some(&def_id), "queued", 0, None, None);
    let duplicate = conn.execute(
        "INSERT INTO jobs (id, def_id, kind, payload, status, attempt, run_after)
         VALUES (?1, ?2, 'catalog_refresh', 'p', 'queued', 0, 0)",
        rusqlite::params![domain::JobId::new().to_string(), def_id],
    );
    assert!(duplicate.is_err(), "同一 def 只能有一个 queued");

    // running 部分唯一索引同样保留：同一 def 不能有两个 running。
    insert_job(&conn, Some(&def_id), "running", 0, None, None);
    let duplicate_running = conn.execute(
        "INSERT INTO jobs (id, def_id, kind, payload, status, attempt, run_after)
         VALUES (?1, ?2, 'catalog_refresh', 'p', 'running', 0, 0)",
        rusqlite::params![domain::JobId::new().to_string(), def_id],
    );
    assert!(duplicate_running.is_err(), "同一 def 只能有一个 running");
}
