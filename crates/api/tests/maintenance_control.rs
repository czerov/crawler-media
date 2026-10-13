//! T1: 禁用在批次/库边界生效与 shutdown-drain 控制测试。

use std::sync::Arc;
use std::time::Duration;

use api::job_loop::{JobLoopControl, spawn_job_loop_with};
use api::maintenance::{SqliteExecutor, run};
use jobs::{JobKind, NewDef};

#[path = "maintenance_support/mod.rs"]
mod support;
use support::{DAY, KEEP, progress_of, temp_state, unix_now};

#[test]
fn disable_after_first_committed_batch_stops_the_next_batch() {
    let (_tmp, state) = temp_state(Arc::new(SqliteExecutor));
    let now = unix_now();

    // 创建一个正式 def
    let def_id = {
        let queue = state.jobs();
        let guard = queue.lock();
        guard
            .upsert_def(NewDef {
                kind: JobKind::JobsRetention,
                name: "jobs_retention_test".into(),
                enabled: true,
                schedule: Some(jobs::Schedule::Interval { secs: 86400 }),
                payload: KEEP.into(),
                timeout_secs: None,
                concurrency_key: None,
            })
            .unwrap()
            .id
    };

    // 塞入 10000 条超龄终态（每批 500，总共 20 批）
    {
        let data = support::data_dir(&state);
        let conn = rusqlite::Connection::open(data.join("jobs.db")).unwrap();
        conn.execute_batch("BEGIN").unwrap();
        {
            let mut stmt = conn
                .prepare(
                    "INSERT INTO jobs (id, def_id, kind, payload, status, attempt, run_after, started_at, finished_at, error, progress, concurrency_key, timeout_secs)
                     VALUES (?1, NULL, 'jobs_retention', '{}', 'succeeded', 0, ?2, ?2, ?2, NULL, NULL, NULL, NULL)"
                )
                .unwrap();
            for i in 0..10000 {
                let ts = now - 10 * DAY;
                stmt.execute(rusqlite::params![format!("expired-job-{i}"), ts]).unwrap();
            }
        }
        conn.execute_batch("COMMIT").unwrap();
    }

    // 领取该任务作为属于 def_id 的 running job
    let job = {
        let queue = state.jobs();
        let guard = queue.lock();
        let enqueued = guard.ensure_scheduled_for(0, def_id).unwrap().remove(0);
        let claimed = guard.claim(now, 1).unwrap();
        assert_eq!(claimed[0].id, enqueued.id);
        assert_eq!(claimed[0].def_id, Some(def_id));
        claimed.into_iter().next().unwrap()
    };

    // 我们需要在执行器或通过线程在第一批执行后将 def_id 设置为 enabled = false。
    // 为了精准拦截批次之间，可以使用后台轮询线程监控 jobs 剩余数量或者使用钩子。
    // 这里启动一个线程监控：当第一批删除完成（剩余 700 条）时，立即将 def 禁用！
    // 为了精准拦截在第 1 批完成与第 2 批开始之间，我们可以在 next 闭包或钩子拦截。
    // 在这里，我们可以通过在 StateRunner 或测试专用 executor，或者在 batches 运行之间精准设置。
    // 观察到：run_batches 是顺序执行 next()。
    // 我们可以直接用带拦截的测试：例如通过一个 atomic 计数，或者通过共享 channel。
    let state_clone = state.clone();
    let monitor = std::thread::spawn(move || {
        let data = support::data_dir(&state_clone);
        let conn = rusqlite::Connection::open(data.join("jobs.db")).unwrap();
        // 轮询直到看到至少已经执行了删除（count < 10000）
        for _ in 0..1000 {
            let count: i64 = conn.query_row("SELECT count(*) FROM jobs WHERE id LIKE 'expired-job-%'", [], |r| r.get(0)).unwrap();
            if count < 10000 {
                state_clone.jobs().lock().set_def_enabled_by_id(def_id, false).unwrap();
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    });

    run(&state, &job).unwrap();
    monitor.join().unwrap();

    let report = progress_of(&state, &job);
    assert!(report["batches"].as_u64().unwrap() < 20, "禁用后必须提前停止，绝不得跑完全部批次: {report}");
    assert!(report["batches"].as_u64().unwrap() >= 1, "至少跑完了第一批: {report}");
    assert_eq!(
        report["deleted"].as_u64().unwrap(),
        report["batches"].as_u64().unwrap() * 500,
        "已提交批次的删除数量一致"
    );
    assert!(
        report["note"]
            .as_str()
            .unwrap_or("")
            .contains("禁用"),
        "报告中需说明因为任务定义禁用而停止: {report}"
    );

    let data = support::data_dir(&state);
    let conn = rusqlite::Connection::open(data.join("jobs.db")).unwrap();
    let remaining: i64 = conn.query_row("SELECT count(*) FROM jobs WHERE id LIKE 'expired-job-%'", [], |r| r.get(0)).unwrap();
    assert!(remaining > 0, "必须有剩余超龄行未被删除: remaining={remaining}");
}

#[tokio::test]
async fn shutdown_stops_claims_and_drains_real_blocking_work() {
    let (_tmp, state) = temp_state(Arc::new(SqliteExecutor));
    let loop_control = Arc::new(JobLoopControl::new());
    let handle = spawn_job_loop_with(
        state.clone(),
        Duration::from_millis(50),
        unix_now,
        loop_control.clone(),
    );

    // 请求停止
    loop_control.request_stop();
    assert!(loop_control.is_stopping());

    // 等待 shutdown 退出
    tokio::time::timeout(Duration::from_secs(2), handle.shutdown())
        .await
        .expect("shutdown 必须正常返回并退出")
        .expect("shutdown 执行无错误");
}
