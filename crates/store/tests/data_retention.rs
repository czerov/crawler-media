//! DB-05 store 合同：分类历史保留的候选/保护/游标。
//!
//! 断言走公开 seam `Store::prune_data_history`；probe/attempt/sample 的 fixture
//! 用 test-only 原始 SQL 造数（store 没有公开的任意时间戳写入接口），读取断言
//! 仍按真实表内容进行。

use std::path::Path;

use domain::{MediaId, UserId};
use rusqlite::{Connection, params};
use store::{DataRetentionCursor, RetentionClass, Store};

const CUTOFF_SECS: i64 = 1_700_000_000;
const CUTOFF_MS: i64 = CUTOFF_SECS * 1000;

fn open_db(dir: &Path, name: &str) -> Connection {
    Connection::open(dir.join(name)).unwrap()
}

fn metric_count(dir: &Path) -> i64 {
    open_db(dir, "subscribe.db")
        .query_row("SELECT COUNT(*) FROM playback_metrics", [], |row| row.get(0))
        .unwrap()
}

fn insert_probe_job(
    dir: &Path,
    id: &str,
    scope: &str,
    status: &str,
    finished_at_ms: Option<i64>,
) {
    let conn = open_db(dir, "library.db");
    conn.execute(
        "INSERT INTO probe_jobs
            (id, kind, media_id, season, scope_key, status, total, completed,
             succeeded, failed, partial, error, created_at_ms, started_at_ms, finished_at_ms)
         VALUES (?1, 'metadata', 'media-1', NULL, ?2, ?3, 1, 1, 1, 0, 0, NULL, ?4, ?4, ?5)",
        params![id, scope, status, finished_at_ms.unwrap_or(1), finished_at_ms],
    )
    .unwrap();
}

fn insert_unit(dir: &Path, job_id: &str, ledger_id: &str, status: &str) {
    let conn = open_db(dir, "library.db");
    conn.execute(
        "INSERT INTO probe_job_units
            (job_id, ledger_id, kind, force_fingerprint, reuse_fingerprint_cache,
             overwrite_markers, status, error, reuse_media_info_cache)
         VALUES (?1, ?2, 'metadata', 0, 0, 0, ?3, NULL, 0)",
        params![job_id, ledger_id, status],
    )
    .unwrap();
}

fn insert_attempt(dir: &Path, attempt_id: &str, job_id: &str) {
    let conn = open_db(dir, "library.db");
    conn.execute(
        "INSERT INTO fingerprint_attempts
            (attempt_id, job_id, ledger_id, kind, window_start_ms, window_end_ms,
             phase, started_at_ms, finished_at_ms, status, error_kind, metrics_json)
         VALUES (?1, ?2, 'ledger-1', 'intro', 0, 1000, 'capture', 1, 2, 'succeeded', NULL, '{}')",
        params![attempt_id, job_id],
    )
    .unwrap();
}

fn insert_sample(dir: &Path, sample_id: &str, job_id: &str) {
    let conn = open_db(dir, "library.db");
    conn.execute(
        "INSERT INTO fingerprint_samples
            (sample_id, ledger_id, source_version, capture_profile_key, kind,
             window_start_ms, window_end_ms, pcm_duration_ms, fingerprint_json,
             captured_job_id, captured_at_ms, metrics_json)
         VALUES (?1, 'ledger-1', 'v1', 'profile', 'intro', 0, 1000, 1000, '{}', ?2, 1, '{}')",
        params![sample_id, job_id],
    )
    .unwrap();
}

fn probe_job_exists(dir: &Path, id: &str) -> bool {
    open_db(dir, "library.db")
        .query_row("SELECT EXISTS(SELECT 1 FROM probe_jobs WHERE id = ?1)", [id], |row| {
            row.get(0)
        })
        .unwrap()
}

fn attempt_count(dir: &Path) -> i64 {
    open_db(dir, "library.db")
        .query_row("SELECT COUNT(*) FROM fingerprint_attempts", [], |row| row.get(0))
        .unwrap()
}

fn catalog_count(dir: &Path) -> i64 {
    open_db(dir, "catalog.db")
        .query_row("SELECT COUNT(*) FROM catalog_cache", [], |row| row.get(0))
        .unwrap()
}

#[test]
fn qoe_only_deletes_rows_strictly_older_than_cutoff() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let user = UserId::new();
    let media = MediaId::new();
    for recorded_at in [CUTOFF_SECS - 1, CUTOFF_SECS, CUTOFF_SECS + 1] {
        store
            .insert_metric(user, media, 0, None, None, 0, 0, 0, None, None, 10, recorded_at)
            .unwrap();
    }

    let batch = store
        .prune_data_history(RetentionClass::Qoe, CUTOFF_MS, None, 500, false)
        .unwrap();

    assert_eq!(batch.examined, 1);
    assert_eq!(batch.deleted, 1);
    assert_eq!(batch.protected, 0);
    assert!(!batch.incomplete);
    assert!(batch.resume_after.is_none());
    assert_eq!(metric_count(tmp.path()), 2, "边界秒与更新的行必须保留");
}

#[test]
fn probe_keeps_three_newest_per_scope_and_all_time_ties() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    // scope 内 6 条：最新 3 条为 50、40a、40b；40 秒并列全部保护。
    for (id, ts) in [
        ("a-10", 10),
        ("a-20", 20),
        ("a-30", 30),
        ("a-40a", 40),
        ("a-40b", 40),
        ("a-50", 50),
    ] {
        insert_probe_job(tmp.path(), id, "scope-a", "succeeded", Some(ts));
    }

    let batch = store
        .prune_data_history(RetentionClass::ProbeHistory, 1000, None, 500, false)
        .unwrap();

    assert_eq!(batch.examined, 6);
    assert_eq!(batch.protected, 3, "40a/40b/50 必须保留（时间 tie 全部保护）");
    assert_eq!(batch.deleted, 3);
    assert!(!batch.incomplete);
    for kept in ["a-40a", "a-40b", "a-50"] {
        assert!(probe_job_exists(tmp.path(), kept), "{kept} 必须保留");
    }
    for gone in ["a-10", "a-20", "a-30"] {
        assert!(!probe_job_exists(tmp.path(), gone), "{gone} 应被删除");
    }
}

#[test]
fn captured_sample_and_active_unit_protect_probe_jobs() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    for (id, ts) in [
        ("b-target", 10),
        ("b-20", 20),
        ("b-30", 30),
        ("b-40", 40),
        ("b-50", 50),
        ("b-60", 60),
    ] {
        insert_probe_job(tmp.path(), id, "scope-b", "succeeded", Some(ts));
    }
    insert_sample(tmp.path(), "sample-1", "b-target");
    // 终态 job 但仍有活动 unit：保守保护。
    insert_probe_job(tmp.path(), "b-unit", "scope-b2", "failed", Some(10));
    insert_unit(tmp.path(), "b-unit", "ledger-1", "running");
    for (id, ts) in [("b2-20", 20), ("b2-30", 30), ("b2-40", 40)] {
        insert_probe_job(tmp.path(), id, "scope-b2", "succeeded", Some(ts));
    }
    insert_probe_job(tmp.path(), "b-active", "scope-b3", "running", None);
    // 终态 job 仍被 probe stage state 引用：不得删除留下悬空引用。
    for (id, ts) in [("b-stage", 10), ("b4-20", 20), ("b4-30", 30), ("b4-40", 40)] {
        insert_probe_job(tmp.path(), id, "scope-b4", "succeeded", Some(ts));
    }
    {
        let conn = open_db(tmp.path(), "library.db");
        conn.execute(
            "INSERT INTO probe_stage_state
                (ledger_id, context_key, stage, status, updated_at_ms, active_job_id)
             VALUES ('ledger-stage', 'ctx', 'metadata', 'running', 1, 'b-stage')",
            [],
        )
        .unwrap();
    }

    let batch = store
        .prune_data_history(RetentionClass::ProbeHistory, 1000, None, 500, false)
        .unwrap();

    // b-target 被 sample 引用；b-40/50/60 是最新 3 条；b-unit 有活动 unit；
    // b-active 不是终态。可删的只有 b-20/b-30。
    assert_eq!(batch.examined, 14);
    assert_eq!(batch.protected, 12);
    assert_eq!(batch.deleted, 2);
    for kept in [
        "b-target", "b-40", "b-50", "b-60", "b-unit", "b2-20", "b2-30", "b2-40", "b-active",
        "b-stage", "b4-20", "b4-30", "b4-40",
    ] {
        assert!(probe_job_exists(tmp.path(), kept), "{kept} 必须保留");
    }
    assert!(!probe_job_exists(tmp.path(), "b-20"));
    assert!(!probe_job_exists(tmp.path(), "b-30"));
}

#[test]
fn attempts_are_deleted_in_the_same_transaction_as_the_job() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    for (id, ts) in [("tx-target", 10), ("tx-20", 20), ("tx-30", 30), ("tx-40", 40), ("tx-50", 50), ("tx-60", 60)] {
        insert_probe_job(tmp.path(), id, "scope-tx", "succeeded", Some(ts));
    }
    insert_attempt(tmp.path(), "attempt-1", "tx-target");
    insert_attempt(tmp.path(), "attempt-2", "tx-target");

    let batch = store
        .prune_data_history(RetentionClass::ProbeHistory, 1000, None, 500, false)
        .unwrap();
    assert_eq!(batch.deleted, 3, "tx-target/tx-20/tx-30 可删");
    assert!(!probe_job_exists(tmp.path(), "tx-target"));
    assert_eq!(attempt_count(tmp.path()), 0, "job 删除必须级联它的 attempts");

    // 用触发器证明 attempts 删除失败时 job 也不会被删（同一事务回滚）。
    for (id, ts) in [("rb-target", 10), ("rb-20", 20), ("rb-30", 30), ("rb-40", 40), ("rb-50", 50), ("rb-60", 60)] {
        insert_probe_job(tmp.path(), id, "scope-rb", "succeeded", Some(ts));
    }
    insert_attempt(tmp.path(), "attempt-rb", "rb-target");
    {
        let conn = open_db(tmp.path(), "library.db");
        conn.execute_batch(
            "CREATE TRIGGER block_attempt_delete BEFORE DELETE ON fingerprint_attempts
             WHEN OLD.job_id = 'rb-target'
             BEGIN SELECT RAISE(ABORT, 'blocked for rollback test'); END;",
        )
        .unwrap();
    }
    let error = store
        .prune_data_history(RetentionClass::ProbeHistory, 1000, None, 500, false)
        .unwrap_err();
    assert!(error.to_string().contains("blocked"), "{error}");
    assert!(probe_job_exists(tmp.path(), "rb-target"), "attempts 删除失败必须回滚 job 删除");
    assert_eq!(attempt_count(tmp.path()), 1);
    {
        let conn = open_db(tmp.path(), "library.db");
        conn.execute_batch("DROP TRIGGER block_attempt_delete;").unwrap();
    }
    let retried = store
        .prune_data_history(RetentionClass::ProbeHistory, 1000, None, 500, false)
        .unwrap();
    assert_eq!(retried.deleted, 3);
    assert!(!probe_job_exists(tmp.path(), "rb-target"));
    assert_eq!(attempt_count(tmp.path()), 0);
}

#[test]
fn transient_catalog_deletes_only_allowlisted_paths_and_protects_details_and_unknowns() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let old = CUTOFF_SECS - 1;
    for (key, fetched_at) in [
        ("/search/movie?query=avatar", old),
        ("/discover/movie?with_genres=28", old),
        ("/trending/movie/day", old),
        ("/j/search_subjects?type=movie", old),
        ("/movie/42", old),
        ("/tv/42?language=zh-CN", old),
        ("/movie/42/credits", old),
        ("search:movie:avatar", old),
        ("/search/tv?query=fresh", CUTOFF_SECS + 1),
    ] {
        store
            .put_catalog_cache("tmdb", key, "{}", fetched_at, None)
            .unwrap();
    }

    let batch = store
        .prune_data_history(
            RetentionClass::TransientCatalog,
            CUTOFF_MS,
            None,
            500,
            false,
        )
        .unwrap();

    // 超龄行 8 条：4 条 transient 可删，3 条详情 + 1 条未知 key 保护；
    // 更新的一行不在窗口内。
    assert_eq!(batch.examined, 8);
    assert_eq!(batch.deleted, 4);
    assert_eq!(batch.protected, 4);
    assert!(!batch.incomplete);
    assert_eq!(catalog_count(tmp.path()), 5);
    for kept in [
        "/movie/42",
        "/tv/42?language=zh-CN",
        "/movie/42/credits",
        "search:movie:avatar",
        "/search/tv?query=fresh",
    ] {
        assert!(
            store
                .get_catalog_cache("tmdb", kept)
                .unwrap()
                .is_some(),
            "{kept} 必须保留"
        );
    }
    assert!(store.get_catalog_cache("tmdb", "/search/movie?query=avatar").unwrap().is_none());
}

#[test]
fn dry_run_reports_the_same_window_without_writing() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let user = UserId::new();
    let media = MediaId::new();
    for _ in 0..3 {
        store
            .insert_metric(user, media, 0, None, None, 0, 0, 0, None, None, 10, CUTOFF_SECS - 10)
            .unwrap();
    }

    let batch = store
        .prune_data_history(RetentionClass::Qoe, CUTOFF_MS, None, 500, true)
        .unwrap();

    assert_eq!(batch.examined, 3);
    assert_eq!(batch.deleted, 0);
    assert_eq!(metric_count(tmp.path()), 3, "dry_run 不得写任何行");
}

#[test]
fn keyset_cursor_pages_without_duplicates_and_stops_at_the_end() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let user = UserId::new();
    let media = MediaId::new();
    for index in 0..1200 {
        store
            .insert_metric(
                user,
                media,
                0,
                None,
                None,
                0,
                0,
                0,
                None,
                None,
                10,
                CUTOFF_SECS - 2000 + index,
            )
            .unwrap();
    }

    let first = store
        .prune_data_history(RetentionClass::Qoe, CUTOFF_MS, None, 500, false)
        .unwrap();
    assert_eq!((first.examined, first.deleted, first.incomplete), (500, 500, true));
    let cursor = first.resume_after.expect("窗口未完成必须返回游标");

    let second = store
        .prune_data_history(RetentionClass::Qoe, CUTOFF_MS, Some(&cursor), 500, false)
        .unwrap();
    assert_eq!((second.examined, second.deleted, second.incomplete), (500, 500, true));
    let cursor = second.resume_after.unwrap();

    let third = store
        .prune_data_history(RetentionClass::Qoe, CUTOFF_MS, Some(&cursor), 500, false)
        .unwrap();
    assert_eq!((third.examined, third.deleted, third.incomplete), (200, 200, false));
    assert!(third.resume_after.is_none());
    assert_eq!(metric_count(tmp.path()), 0);
}

#[test]
fn cursor_carries_examined_position_when_the_window_is_full_of_protected_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let user = UserId::new();
    let media = MediaId::new();
    for index in 0..3 {
        store
            .insert_metric(user, media, 0, None, None, 0, 0, 0, None, None, 10, CUTOFF_SECS - 10 + index)
            .unwrap();
    }
    let batch = store
        .prune_data_history(RetentionClass::Qoe, CUTOFF_MS, None, 2, false)
        .unwrap();
    assert_eq!(batch.examined, 2);
    assert_eq!(batch.deleted, 2);
    assert!(batch.incomplete);
    let cursor: DataRetentionCursor = batch.resume_after.unwrap();
    assert_eq!(cursor.at_ms, (CUTOFF_SECS - 9) * 1000);
}

#[test]
fn zero_limit_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let error = store
        .prune_data_history(RetentionClass::Qoe, CUTOFF_MS, None, 0, true)
        .unwrap_err();
    assert!(matches!(error, store::StoreError::Invalid(_)), "{error}");
}

