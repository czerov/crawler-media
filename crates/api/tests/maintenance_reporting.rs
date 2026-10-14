//! T3 测试：保护数量截断与重复累加修复测试 (F03)。

use std::sync::Arc;
use api::{ApiState, router};
use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use jobs::JobKind;
use serde_json::{json, Value};
use tower::ServiceExt;

#[path = "maintenance_support/mod.rs"]
mod support;
use support::{DAY, progress_of, temp_state, unix_now};

async fn call_api(
    app: &axum::Router,
    method: &str,
    uri: &str,
    token: &str,
    body: Value,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    if !token.is_empty() {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    let body_data = if body.is_null() {
        Body::empty()
    } else {
        Body::from(body.to_string())
    };
    let response = app
        .clone()
        .oneshot(builder.body(body_data).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let envelope: Value = serde_json::from_slice(&bytes).unwrap();
    (status, envelope)
}

#[tokio::test]
async fn protected_count_in_preview_and_run_does_not_double_count_and_marks_truncation() {
    let (_tmp, state) = temp_state(Arc::new(api::maintenance::SqliteExecutor));
    let now = unix_now();

    // 1. 创建 600 个 def，各插入 1 条超龄终态（全部受 last-child 保护）
    {
        let data = support::data_dir(&state);
        let conn = rusqlite::Connection::open(data.join("jobs.db")).unwrap();
        conn.execute_batch("BEGIN").unwrap();
        for i in 0..600 {
            let def_id = domain::JobDefId::new().to_string();
            let job_id = domain::JobId::new().to_string();
            conn.execute(
                "INSERT INTO job_defs (id, kind, name, enabled, payload) VALUES (?1, 'scrape', 'test', 1, '{}')",
                rusqlite::params![def_id],
            ).unwrap();
            conn.execute(
                "INSERT INTO jobs (id, def_id, kind, payload, status, attempt, run_after, started_at, finished_at)
                 VALUES (?1, ?2, 'scrape', '{}', 'succeeded', 0, ?3, ?3, ?3)",
                rusqlite::params![job_id, def_id, now - 100 * DAY],
            ).unwrap();
        }
        // 2. 插入 1200 条可删独立历史（def_id 为 NULL）
        for i in 0..1200 {
            let job_id = domain::JobId::new().to_string();
            conn.execute(
                "INSERT INTO jobs (id, def_id, kind, payload, status, attempt, run_after, started_at, finished_at)
                 VALUES (?1, NULL, 'scrape', '{}', 'succeeded', 0, ?2, ?2, ?2)",
                rusqlite::params![job_id, now - 100 * DAY],
            ).unwrap();
        }
        conn.execute_batch("COMMIT").unwrap();
    }

    let def = state.jobs().lock().list_defs().unwrap()
        .into_iter().find(|d| d.kind == JobKind::JobsRetention).unwrap();

    // 先做一次 preview：600 个保护行（超出了 batch_size=500 的探测窗口），应当截断并标注 protected_truncated / truncated
    let app = router(state.clone());
    let (status, resp) = call_api(
        &app,
        "POST",
        &format!("/api/v1/jobs/{}/maintenance-preview", def.id),
        "maintenance-secret",
        json!({}),
    ).await;
    assert_eq!(status, axum::http::StatusCode::OK, "preview 响应失败: {resp}");
    let data = &resp["data"];
    assert_eq!(data["candidates"], 500, "候选取预算上限 500: {data}");
    assert_eq!(data["protected"], 500, "保护探测最多 500（下界）: {data}");
    assert_eq!(data["truncated"], true, "因为窗口外还有候选/保护行，必须为 true: {data}");
    assert_eq!(data["protected_truncated"], true, "保护行超限必须明确标注截断: {data}");

    // 然后执行任务（1200 条可删任务会分 3 批：500，500，200）
    let job = {
        let queue = state.jobs();
        let guard = queue.lock();
        let enqueued = guard.ensure_scheduled_for(0, def.id).unwrap().remove(0);
        let claimed = guard.claim(now, 1).unwrap();
        claimed.into_iter().next().unwrap()
    };

    api::maintenance::run(&state, &job).unwrap();

    let report = progress_of(&state, &job);
    // B3 §5.1：原始窗口（含保护行）按 keyset 推进，1800 行 = 500+500+500+300 共 4 批；
    // 窗口互不重叠，所以 protected 累加不会重复计数。
    assert_eq!(report["batches"], 4, "共执行 4 批: {report}");
    assert_eq!(report["deleted"], 1200, "成功删除了 1200 条独立任务: {report}");

    // 关键断言：旧实现中每一批都 += 501 导致 protected 累加为 1503！
    // 修复后：每个保护行只被检视一次，累加结果恰好是 600，而不是 1500+。
    let reported_protected = report["protected"].as_u64().unwrap();
    assert_eq!(
        reported_protected, 600,
        "本 cycle 已检视保护行数为 600，不得跨批重复累加: reported={reported_protected}, {report}"
    );
    assert_eq!(
        report["protected_is_lower_bound"], true,
        "窗口外仍有未检视行时必须明确报告为下界: {report}"
    );
}
