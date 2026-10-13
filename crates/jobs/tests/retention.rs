//! 终止态 Jobs 历史的按龄分批保留（DB8 Task 2）。
//!
//! 断言只走公开 API（`Queue::open` / `enqueue` / `claim` / `succeed` /
//! `fail_claimed` / `get` / `get_last_child` / `children_for_defs` /
//! `jobs_for_payload` / `prune_terminal_jobs`）。少数 fixture 是公开 API
//! 无法产生的状态（终态但 `finished_at IS NULL`、`job_defs` 已删除的 def 历史、
//! 大量批量数据、失败注入 trigger），用测试专用 SQL 造；它们不是断言对象。

use std::path::PathBuf;
use std::time::Duration;

use domain::{JobDefId, JobId};
use jobs::{JobError, JobKind, JobStatus, NewDef, Queue};
use rusqlite::Connection;

struct Fixture {
    _dir: tempfile::TempDir,
    path: PathBuf,
    queue: Queue,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("jobs.db");
        let queue = Queue::open(&path).unwrap();
        Self {
            _dir: dir,
            path,
            queue,
        }
    }

    /// 测试专用连接：只用于造公开 API 无法产生的 fixture 或注入失败。
    fn raw(&self) -> Connection {
        let conn = Connection::open(&self.path).unwrap();
        conn.busy_timeout(Duration::from_secs(5)).unwrap();
        conn
    }
}

fn new_def(queue: &Queue, name: &str) -> JobDefId {
    queue
        .upsert_def(NewDef {
            kind: JobKind::CatalogRefresh,
            name: name.into(),
            enabled: true,
            schedule: None,
            payload: format!(r#"{{"name":"{name}"}}"#),
            timeout_secs: None,
            concurrency_key: None,
        })
        .unwrap()
        .id
}

/// 公开 API 造一条终态独立 job（`def_id = NULL`）。
fn finished_oneshot(queue: &Queue, payload: &str, finished_at: i64) -> JobId {
    let job = queue.enqueue(JobKind::Transfer, payload, 0).unwrap();
    let claimed = queue.claim(0, 1).unwrap();
    assert_eq!(claimed.len(), 1, "claim 必须拿到刚入队的 job");
    assert_eq!(claimed[0].id, job.id);
    queue.succeed(job.id, finished_at).unwrap();
    job.id
}

/// 公开 API 造一条属于 def 的终态 job（`run_after` 固定为 0）。
fn finished_def_job(queue: &Queue, def_id: JobDefId, finished_at: i64) -> JobId {
    let job = queue.ensure_scheduled_for(0, def_id).unwrap().remove(0);
    let claimed = queue.claim(0, 1).unwrap();
    assert_eq!(claimed[0].id, job.id);
    queue.succeed(job.id, finished_at).unwrap();
    job.id
}

/// 测试专用：终态但 `finished_at IS NULL`（公开 API 的 finish 总会写时间戳）。
fn insert_terminal_null_finished_at(fixture: &Fixture) -> JobId {
    insert_raw_job(fixture, None, "succeeded", None)
}

/// 测试专用：任意状态 / `finished_at` 的裸行。
fn insert_raw_job(
    fixture: &Fixture,
    def_id: Option<JobDefId>,
    status: &str,
    finished_at: Option<i64>,
) -> JobId {
    let id = JobId::new();
    fixture
        .raw()
        .execute(
            "INSERT INTO jobs (id, def_id, kind, payload, status, attempt, run_after,
                 started_at, finished_at, error, progress, concurrency_key, timeout_secs)
             VALUES (?1, ?2, 'transfer', 'raw', ?3, 0, 0, NULL, ?4, NULL, NULL, NULL, NULL)",
            rusqlite::params![
                id.to_string(),
                def_id.map(|def_id| def_id.to_string()),
                status,
                finished_at
            ],
        )
        .unwrap();
    id
}

fn last_ids(queue: &Queue, ids: &[JobDefId]) -> Vec<Option<JobId>> {
    ids.iter()
        .map(|id| queue.get_last_child(*id).unwrap().map(|job| job.id))
        .collect()
}

#[test]
fn deletes_expired_terminal_and_keeps_boundary_null_and_live() {
    let fixture = Fixture::new();
    let queue = &fixture.queue;
    const CUTOFF: i64 = 1_000;

    let expired = finished_oneshot(queue, "expired", 400);
    let boundary = finished_oneshot(queue, "boundary", CUTOFF);
    let fresh = finished_oneshot(queue, "fresh", CUTOFF + 1);
    let null_finished = insert_terminal_null_finished_at(&fixture);

    // queued / running 永不删除：queued 用远期 run_after 避免被 claim。
    let queued = queue
        .enqueue(JobKind::Transfer, "queued", 1_000_000)
        .unwrap();
    let running = queue.enqueue(JobKind::Transfer, "running", 0).unwrap();
    assert_eq!(queue.claim(0, 1).unwrap()[0].id, running.id);

    let batch = queue.prune_terminal_jobs(CUTOFF, 500, false).unwrap();
    assert_eq!(batch.scanned, 1, "只检视超龄终态行");
    assert_eq!(batch.candidates, 1);
    assert_eq!(batch.protected, 0);
    assert_eq!(batch.deleted, 1);
    assert!(!batch.has_more);

    assert!(
        queue.get(expired).unwrap().is_none(),
        "超龄 terminal 必须删除"
    );
    assert!(
        queue.get(boundary).unwrap().is_some(),
        "finished_at == cutoff 保留"
    );
    assert!(queue.get(fresh).unwrap().is_some());
    assert!(
        queue.get(null_finished).unwrap().is_some(),
        "finished_at IS NULL 保留"
    );
    assert!(queue.get(queued.id).unwrap().is_some(), "queued 保留");
    assert!(queue.get(running.id).unwrap().is_some(), "running 保留");
}

#[test]
fn keeps_latest_terminal_per_def_and_full_tie_group() {
    let fixture = Fixture::new();
    let queue = &fixture.queue;
    const CUTOFF: i64 = 10_000;

    // delta：两条终态，最新一条是 last child。
    let delta = new_def(queue, "delta");
    let d_old = finished_def_job(queue, delta, 100);
    let d_latest = finished_def_job(queue, delta, 200);

    // echo：finished_at 与 run_after 完全并列，整组保留。
    let echo = new_def(queue, "echo");
    let e_tie_a = finished_def_job(queue, echo, 300);
    let e_tie_b = finished_def_job(queue, echo, 300);

    // foxtrot：最新处并列，同时有更老的候选。
    let foxtrot = new_def(queue, "foxtrot");
    let f_old = finished_def_job(queue, foxtrot, 100);
    let f_tie_a = finished_def_job(queue, foxtrot, 400);
    let f_tie_b = finished_def_job(queue, foxtrot, 400);

    // golf：最新一条未超龄（不是候选），更老的一条仍是候选并可删。
    let golf = new_def(queue, "golf");
    let g_old = finished_def_job(queue, golf, 100);
    let g_new = finished_def_job(queue, golf, CUTOFF + 1);

    // hotel：只有一条终态历史，必须作为 last child 保留。
    let hotel = new_def(queue, "hotel");
    let h_only = finished_def_job(queue, hotel, 500);

    let defs = [delta, echo, foxtrot, golf, hotel];
    let before = last_ids(queue, &defs);

    let batch = queue.prune_terminal_jobs(CUTOFF, 500, false).unwrap();
    // 候选窗口只含**可删**行（d_old / f_old / g_old）；被 last-child 保护的行不再进入
    // 窗口，而由 `protected` 单独报告——这正是 DB8 复核 C1 的修复方式。
    assert_eq!(batch.scanned, 3, "窗口只含可删行");
    assert_eq!(batch.candidates, 3);
    assert_eq!(batch.protected, 6, "delta 1 + echo 2 + foxtrot 2 + hotel 1");
    assert_eq!(batch.deleted, 3);
    assert!(!batch.has_more);

    assert!(queue.get(d_old).unwrap().is_none());
    assert!(queue.get(d_latest).unwrap().is_some());
    assert!(queue.get(e_tie_a).unwrap().is_some(), "并列组必须整组保留");
    assert!(queue.get(e_tie_b).unwrap().is_some(), "并列组必须整组保留");
    assert!(queue.get(f_old).unwrap().is_none());
    assert!(queue.get(f_tie_a).unwrap().is_some(), "最新并列组整组保留");
    assert!(queue.get(f_tie_b).unwrap().is_some(), "最新并列组整组保留");
    assert!(
        queue.get(g_old).unwrap().is_none(),
        "last child 不是候选也可删"
    );
    assert!(queue.get(g_new).unwrap().is_some());
    assert!(
        queue.get(h_only).unwrap().is_some(),
        "单条历史即 last child"
    );

    assert_eq!(last_ids(queue, &defs), before, "last child 不得改变");
    assert_eq!(queue.list_defs().unwrap().len(), 5, "job_defs 不删除");
}

#[test]
fn keeps_non_terminal_rows_even_with_stale_finished_at() {
    let fixture = Fixture::new();
    let queue = &fixture.queue;
    const CUTOFF: i64 = 10_000;

    // 公开 API 的正在重试：queued + finished_at NULL。
    let retry = queue.enqueue(JobKind::Transfer, "retry", 0).unwrap();
    let claimed = queue.claim(0, 1).unwrap().remove(0);
    let retried = queue.fail_claimed(&claimed, 50, "boom").unwrap().unwrap();
    assert_eq!(retried.status, JobStatus::Queued);
    assert!(retried.finished_at.is_none());

    // 测试专用：queued/running 但 finished_at 已超龄，证明状态规则独立于 NULL。
    let queued_stale = insert_raw_job(&fixture, None, "queued", Some(100));
    let running_stale = insert_raw_job(&fixture, None, "running", Some(100));

    let batch = queue.prune_terminal_jobs(CUTOFF, 500, false).unwrap();
    assert_eq!(batch.scanned, 0);
    assert_eq!(batch.candidates, 0);
    assert_eq!(batch.deleted, 0);

    assert!(
        queue.get(retry.id).unwrap().is_some(),
        "重试中的 queued 保留"
    );
    assert!(queue.get(queued_stale).unwrap().is_some(), "queued 保留");
    assert!(queue.get(running_stale).unwrap().is_some(), "running 保留");
}

#[test]
fn deletes_def_id_null_history_independently_by_age() {
    let fixture = Fixture::new();
    let queue = &fixture.queue;
    const CUTOFF: i64 = 1_000;

    let old = finished_oneshot(queue, "old", 100);
    let mid = finished_oneshot(queue, "mid", 900);
    let keep = finished_oneshot(queue, "keep", 1_500);

    let batch = queue.prune_terminal_jobs(CUTOFF, 500, false).unwrap();
    assert_eq!(
        batch.candidates, 2,
        "def_id IS NULL 的历史不共享 last-child 保护"
    );
    assert_eq!(batch.protected, 0);
    assert_eq!(batch.deleted, 2);
    assert!(queue.get(old).unwrap().is_none());
    assert!(queue.get(mid).unwrap().is_none());
    assert!(queue.get(keep).unwrap().is_some());
}

#[test]
fn prune_preserves_last_child_and_children_for_defs() {
    let fixture = Fixture::new();
    let queue = &fixture.queue;
    const CUTOFF: i64 = 10_000;

    let alpha = new_def(queue, "alpha");
    let a_old = finished_def_job(queue, alpha, 100);
    let a_latest = finished_def_job(queue, alpha, 200);
    let a_live = queue.ensure_scheduled_for(0, alpha).unwrap().remove(0);

    // job_defs 已删除、但历史 job 的 def_id 仍非 NULL。
    let orphan = JobDefId::new();
    let o_old = insert_raw_job(&fixture, Some(orphan), "succeeded", Some(100));
    let o_latest = insert_raw_job(&fixture, Some(orphan), "succeeded", Some(300));
    assert!(queue.def(orphan).unwrap().is_none(), "fixture: def 已删除");

    let defs = [alpha, orphan];
    let before_last = last_ids(queue, &defs);
    let before_children = queue.children_for_defs(&defs).unwrap();

    let batch = queue.prune_terminal_jobs(CUTOFF, 500, false).unwrap();
    assert_eq!(batch.deleted, 2, "只删 a_old 与 o_old");

    assert!(queue.get(a_old).unwrap().is_none());
    assert!(queue.get(o_old).unwrap().is_none());
    assert!(queue.get(a_latest).unwrap().is_some());
    assert!(queue.get(o_latest).unwrap().is_some());
    assert!(queue.get(a_live.id).unwrap().is_some(), "live child 保留");

    assert_eq!(last_ids(queue, &defs), before_last, "last child 不得改变");
    let after_children = queue.children_for_defs(&defs).unwrap();
    assert_eq!(after_children.len(), before_children.len());
    for (before, after) in before_children.iter().zip(&after_children) {
        assert_eq!(before.def_id, after.def_id);
        assert_eq!(
            before.live.as_ref().map(|job| job.id),
            after.live.as_ref().map(|job| job.id),
            "live child 不得改变"
        );
        assert_eq!(
            before.last.as_ref().map(|job| job.id),
            after.last.as_ref().map(|job| job.id),
            "last child 不得改变"
        );
    }
}

#[test]
fn batch_size_caps_each_call_and_has_more_tracks_remaining_candidates() {
    let fixture = Fixture::new();
    let queue = &fixture.queue;
    const CUTOFF: i64 = 10_000;

    let ids: Vec<JobId> = (0..6)
        .map(|index| finished_oneshot(queue, &format!("bulk-{index}"), 100 + index))
        .collect();

    // 每批 2 行：6 条候选 = 3 批；第 3 批恰好填满且已无剩余 → has_more=false。
    let expected = [
        (2_usize, 2_usize, 2_usize, true),
        (2, 2, 2, true),
        (2, 2, 2, false),
        (0, 0, 0, false),
    ];
    for (call, (scanned, candidates, deleted, has_more)) in expected.into_iter().enumerate() {
        let batch = queue.prune_terminal_jobs(CUTOFF, 2, false).unwrap();
        assert_eq!(
            (
                batch.scanned,
                batch.candidates,
                batch.deleted,
                batch.has_more
            ),
            (scanned, candidates, deleted, has_more),
            "call {call}"
        );
    }
    for id in ids {
        assert!(queue.get(id).unwrap().is_none(), "{id} 必须被删");
    }
}

#[test]
fn dry_run_reports_candidates_without_writing() {
    let fixture = Fixture::new();
    let queue = &fixture.queue;
    const CUTOFF: i64 = 10_000;

    let ids: Vec<JobId> = (0..3)
        .map(|index| finished_oneshot(queue, "dry", 100 + index))
        .collect();
    assert_eq!(queue.jobs_for_payload("dry", 100).len(), 3);

    let batch = queue.prune_terminal_jobs(CUTOFF, 500, true).unwrap();
    assert_eq!(batch.scanned, 3);
    assert_eq!(batch.candidates, 3);
    assert_eq!(batch.deleted, 0, "dry_run 不删除");
    assert_eq!(batch.protected, 0);
    assert!(!batch.has_more);
    assert_eq!(
        queue.jobs_for_payload("dry", 100).len(),
        3,
        "dry_run 必须零写入"
    );
    for id in &ids {
        assert!(queue.get(*id).unwrap().is_some());
    }

    let real = queue.prune_terminal_jobs(CUTOFF, 500, false).unwrap();
    assert_eq!(real.deleted, 3);
    assert_eq!(queue.jobs_for_payload("dry", 100).len(), 0);
}

#[test]
fn delete_failure_rolls_back_the_whole_batch_and_returns_err() {
    let fixture = Fixture::new();
    let queue = &fixture.queue;
    const CUTOFF: i64 = 10_000;

    // 400 条可删候选 + 1 条注入失败的候选（finished_at 最大 → 落在第二个 DELETE 分片）。
    for index in 0..400 {
        finished_oneshot(queue, "bulk", 100 + index);
    }
    let blocked = finished_oneshot(queue, "blocked", CUTOFF - 1);

    fixture
        .raw()
        .execute_batch(
            "CREATE TRIGGER retention_block BEFORE DELETE ON jobs
             WHEN OLD.payload = 'blocked'
             BEGIN SELECT RAISE(ABORT, 'injected delete failure'); END;",
        )
        .unwrap();

    let error = queue.prune_terminal_jobs(CUTOFF, 500, false).unwrap_err();
    assert!(
        matches!(error, JobError::Sqlite(_)),
        "必须返回 SQLite 错误: {error:?}"
    );

    assert_eq!(
        queue.jobs_for_payload("bulk", 500).len(),
        400,
        "失败前已执行的分片也必须随事务回滚"
    );
    assert!(queue.get(blocked).unwrap().is_some());
}

/// DB8 复核 C1 回归：候选窗口按 `finished_at` 从旧到新取，如果**最旧的一批恰好全是
/// 各自 def 的最新终态行**（因此被 last-child 保护），旧实现会把它们选进窗口、算出
/// `deleted = 0`，调用方据此认为"没有可删候选"而永久停止——清理静默失效却报告成功。
///
/// 修复方式：把「可删」谓词并入候选窗口，使窗口只含真正能删的行。
#[test]
fn progresses_even_when_the_oldest_terminal_rows_are_all_protected() {
    let fixture = Fixture::new();
    let queue = &fixture.queue;

    // 600 个 def，各只有一条超龄终态行 → 全部是各自 def 的最新行 → 全部受保护。
    for index in 0..600 {
        let def = new_def(queue, &format!("protected-{index}"));
        insert_raw_job(&fixture, Some(def), "succeeded", Some(1_000));
    }
    // 一个 def 有两条超龄行：旧的那条可删，新的那条受保护。
    let mixed = new_def(queue, "mixed");
    let deletable = insert_raw_job(&fixture, Some(mixed), "succeeded", Some(500_000));
    let protected = insert_raw_job(&fixture, Some(mixed), "succeeded", Some(500_100));

    let cutoff = 600_000;
    let batch = queue.prune_terminal_jobs(cutoff, 500, false).unwrap();
    assert_eq!(
        batch.deleted, 1,
        "最旧的一批全被保护时仍必须推进到可删行（否则清理永久失效）: {batch:?}"
    );
    assert!(
        queue.get(deletable).unwrap().is_none(),
        "可删的旧行应被删除"
    );
    assert!(
        queue.get(protected).unwrap().is_some(),
        "受 last-child 保护的行必须保留"
    );
    assert_eq!(
        queue.get_last_child(mixed).unwrap().unwrap().id,
        protected,
        "last-child 语义不变"
    );

    // 推进一次后确实没有更多可删候选：再调用应报告 deleted=0 且 has_more=false。
    let again = queue.prune_terminal_jobs(cutoff, 500, false).unwrap();
    assert_eq!(again.deleted, 0);
    assert!(!again.has_more, "没有可删候选时 has_more 必须为 false");
}
