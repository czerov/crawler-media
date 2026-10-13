use domain::{JobDefId, JobId};
use rusqlite::{Connection, OptionalExtension, params};

use crate::kinds::{JobKind, Schedule};
use crate::queue::NewDef;
use crate::rows::{JOB_COLS, map_job};
use crate::types::{Job, JobDef, JobError, JobStatus};

mod lifecycle;
pub use lifecycle::{fail, fail_claimed, finish_claimed, recover, recover_except};
pub fn migrate(conn: &Connection) -> Result<(), JobError> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS job_defs (
            id TEXT PRIMARY KEY,
            kind TEXT NOT NULL,
            name TEXT NOT NULL,
            enabled INTEGER NOT NULL,
            schedule TEXT,
            payload TEXT NOT NULL,
            timeout_secs INTEGER,
            concurrency_key TEXT
        );
        CREATE TABLE IF NOT EXISTS jobs (
            id TEXT PRIMARY KEY,
            def_id TEXT,
            kind TEXT NOT NULL,
            payload TEXT NOT NULL,
            status TEXT NOT NULL,
            attempt INTEGER NOT NULL DEFAULT 0,
            run_after INTEGER NOT NULL,
            started_at INTEGER,
            finished_at INTEGER,
            error TEXT,
            progress TEXT,
            concurrency_key TEXT,
            timeout_secs INTEGER
        );
        DROP INDEX IF EXISTS jobs_live_def;
        CREATE UNIQUE INDEX IF NOT EXISTS jobs_running_def
            ON jobs(def_id) WHERE def_id IS NOT NULL AND status = 'running';
        CREATE UNIQUE INDEX IF NOT EXISTS jobs_queued_def
            ON jobs(def_id) WHERE def_id IS NOT NULL AND status = 'queued';
        CREATE INDEX IF NOT EXISTS jobs_status_run_after_idx
            ON jobs(status, run_after) WHERE status = 'queued';
        CREATE INDEX IF NOT EXISTS jobs_payload_idx ON jobs(payload);
        CREATE INDEX IF NOT EXISTS jobs_finished_idx ON jobs(finished_at)
            WHERE finished_at IS NOT NULL;
        -- DB5：热查询索引。20 万行合成数据 + ANALYZE 的 before/after EQP 与耗时见
        -- docs/audits/2026-10-12-remediation-review-repro/jobs-query-plans.md。
        -- 现有 jobs_running_def/jobs_queued_def 是部分**唯一**索引，status IN (...)
        -- 不蕴含 status='queued'，帮不上 live/last child；两者都必须保留。
        CREATE INDEX IF NOT EXISTS jobs_running_status_idx
            ON jobs(status) WHERE status = 'running';
        CREATE INDEX IF NOT EXISTS jobs_running_key_idx
            ON jobs(concurrency_key) WHERE status = 'running';
        CREATE INDEX IF NOT EXISTS jobs_live_def_idx
            ON jobs(def_id, status, run_after) WHERE status IN ('queued','running');
        CREATE INDEX IF NOT EXISTS jobs_last_def_idx
            ON jobs(def_id, finished_at DESC, run_after DESC)
            WHERE status IN ('succeeded','failed','cancelled');
        "#,
    )?;
    Ok(())
}

pub fn insert_job(
    conn: &Connection,
    def_id: Option<JobDefId>,
    kind: JobKind,
    payload: &str,
    run_after: i64,
    concurrency_key: Option<&str>,
    timeout_secs: Option<u32>,
) -> Result<Job, JobError> {
    let job = Job {
        id: JobId::new(),
        def_id,
        kind,
        payload: payload.to_string(),
        status: JobStatus::Queued,
        attempt: 0,
        run_after,
        started_at: None,
        finished_at: None,
        error: None,
        progress: None,
        concurrency_key: concurrency_key.map(str::to_string),
        timeout_secs,
    };
    conn.execute(
        "INSERT INTO jobs (
            id, def_id, kind, payload, status, attempt, run_after,
            started_at, finished_at, error, progress, concurrency_key, timeout_secs
         ) VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6, NULL, NULL, NULL, NULL, ?7, ?8)",
        params![
            job.id.to_string(),
            job.def_id.map(|id| id.to_string()),
            job.kind.as_str(),
            job.payload,
            job.status.as_str(),
            job.run_after,
            job.concurrency_key,
            job.timeout_secs,
        ],
    )?;
    Ok(job)
}

pub fn upsert_def(conn: &Connection, def: NewDef) -> Result<JobDef, JobError> {
    let stored = JobDef {
        id: JobDefId::new(),
        kind: def.kind,
        name: def.name,
        enabled: def.enabled,
        schedule: def.schedule,
        payload: def.payload,
        timeout_secs: def.timeout_secs,
        concurrency_key: def.concurrency_key,
    };
    conn.execute(
        "INSERT INTO job_defs (
            id, kind, name, enabled, schedule, payload, timeout_secs, concurrency_key
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            stored.id.to_string(),
            stored.kind.as_str(),
            stored.name,
            stored.enabled as i64,
            stored.schedule.as_ref().map(Schedule::as_str),
            stored.payload,
            stored.timeout_secs,
            stored.concurrency_key,
        ],
    )?;
    Ok(stored)
}

pub fn ensure_def(conn: &Connection, def: NewDef) -> Result<JobDef, JobError> {
    let key = def.concurrency_key.clone();
    let kind = def.kind.as_str().to_string();
    let existing = crate::schedule::list_defs(conn)?
        .into_iter()
        .find(|row| row.kind.as_str() == kind && row.concurrency_key == key);
    if let Some(existing) = existing {
        return Ok(existing);
    }
    upsert_def(conn, def)
}

pub fn claim(conn: &Connection, now: i64, limit: usize) -> Result<Vec<Job>, JobError> {
    let mut claimed = Vec::new();
    for _ in 0..limit {
        let Some(job) = next_claimable(conn, now)? else {
            break;
        };
        let changed = conn.execute(
            "UPDATE jobs SET status = 'running', started_at = ?1
             WHERE id = ?2 AND status = 'queued'",
            params![now, job.id.to_string()],
        )?;
        if changed == 1 {
            if let Some(taken) = get_job(conn, job.id)? {
                claimed.push(taken);
            }
        }
    }
    Ok(claimed)
}

fn next_claimable(conn: &Connection, now: i64) -> Result<Option<Job>, JobError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {JOB_COLS} FROM jobs
         WHERE status = 'queued' AND run_after <= ?1
         ORDER BY run_after, id"
    ))?;
    let rows = stmt.query_map(params![now], map_job)?;
    for row in rows {
        let job = row?;
        if let Some(def_id) = job.def_id {
            let enabled: bool = conn
                .query_row(
                    "SELECT enabled FROM job_defs WHERE id = ?1",
                    params![def_id.to_string()],
                    |r| r.get(0),
                )
                .unwrap_or(true);
            if !enabled {
                continue;
            }
        }
        if !key_busy(conn, job.concurrency_key.as_deref(), job.def_id, job.id)? {
            return Ok(Some(job));
        }
    }
    Ok(None)
}

/// 该 job 的 concurrency_key / def_id 是否已被别的 running job 占用。
///
/// 拆成两次索引探测（原来是单条 OR 查询；before/after EQP 与耗时见
/// `docs/audits/2026-10-12-remediation-review-repro/jobs-query-plans.md`）。
/// 布尔语义与原式 `(?1 IS NOT NULL AND …) OR (?2 IS NOT NULL AND …)` 完全一致：
/// key 为 NULL 时只看 def_id，def_id 为 NULL 时只看 key，两者都为 NULL 时永远不忙。
fn key_busy(
    conn: &Connection,
    key: Option<&str>,
    def_id: Option<JobDefId>,
    except: JobId,
) -> Result<bool, JobError> {
    let except = except.to_string();
    if let Some(key) = key {
        let busy: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM jobs
             WHERE status = 'running' AND id != ?1 AND concurrency_key = ?2)",
            params![except, key],
            |row| row.get(0),
        )?;
        if busy {
            return Ok(true);
        }
    }
    if let Some(def_id) = def_id {
        let busy: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM jobs
             WHERE status = 'running' AND id != ?1 AND def_id = ?2)",
            params![except, def_id.to_string()],
            |row| row.get(0),
        )?;
        if busy {
            return Ok(true);
        }
    }
    Ok(false)
}

pub fn get_job(conn: &Connection, id: JobId) -> Result<Option<Job>, JobError> {
    conn.query_row(
        &format!("SELECT {JOB_COLS} FROM jobs WHERE id = ?1"),
        params![id.to_string()],
        map_job,
    )
    .optional()
    .map_err(Into::into)
}

/// Delete job defs whose payload matches exactly, and their child jobs.
pub fn delete_defs_for_payload(conn: &Connection, payload: &str) -> Result<usize, JobError> {
    let ids: Vec<String> = conn
        .prepare("SELECT id FROM job_defs WHERE payload = ?1")?
        .query_map(params![payload], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut removed = 0;
    for id in &ids {
        conn.execute("DELETE FROM jobs WHERE def_id = ?1", params![id])?;
        removed += conn.execute("DELETE FROM job_defs WHERE id = ?1", params![id])?;
    }
    Ok(removed)
}

/// Recent child jobs (any status) whose payload matches, newest first.
pub fn jobs_for_payload(
    conn: &Connection,
    payload: &str,
    limit: usize,
) -> Result<Vec<Job>, JobError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {JOB_COLS} FROM jobs WHERE payload = ?1 ORDER BY run_after DESC LIMIT ?2"
    ))?;
    let rows = stmt.query_map(params![payload, limit as i64], map_job)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

/// `set_def_payload_by_id` 的并发结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PayloadUpdate {
    /// 比较通过并已写入新 payload。
    Updated,
    /// def 不存在。
    Missing,
    /// 服务端当前 payload 与调用方给的期望值不同（并发修改），未写入。
    Conflict,
}

/// Compare-and-set a def's payload in one transaction.
///
/// 并发令牌就是**保存前的原 payload**，不是 payload 里那个 schema `version` 字段
/// （`version` 只描述 JSON 结构，不随每次修改变化）。比较按 JSON 结构进行，
/// 因此客户端直接回传自己读到的 `payload` 对象即可；服务端仍以比较时的真实值为准，
/// 不接受仅凭请求里的期望值就写入。
pub fn set_def_payload_by_id(
    conn: &Connection,
    def_id: JobDefId,
    expected: &str,
    payload: &str,
) -> Result<PayloadUpdate, JobError> {
    let tx = conn.unchecked_transaction()?;
    let current: Option<String> = tx
        .query_row(
            "SELECT payload FROM job_defs WHERE id = ?1",
            params![def_id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    let Some(current) = current else {
        return Ok(PayloadUpdate::Missing);
    };
    if !same_payload(&current, expected) {
        return Ok(PayloadUpdate::Conflict);
    }
    tx.execute(
        "UPDATE job_defs SET payload = ?1 WHERE id = ?2",
        params![payload, def_id.to_string()],
    )?;
    tx.commit()?;
    Ok(PayloadUpdate::Updated)
}

/// 按 JSON 结构比较两段 payload；任一侧不是合法 JSON 时退化为逐字比较。
fn same_payload(current: &str, expected: &str) -> bool {
    match (
        serde_json::from_str::<serde_json::Value>(current),
        serde_json::from_str::<serde_json::Value>(expected),
    ) {
        (Ok(current), Ok(expected)) => current == expected,
        _ => current == expected,
    }
}

pub fn set_def_enabled_by_id(
    conn: &Connection,
    def_id: JobDefId,
    enabled: bool,
) -> Result<usize, JobError> {
    let changed = conn.execute(
        "UPDATE job_defs SET enabled = ?1 WHERE id = ?2",
        params![enabled as i64, def_id.to_string()],
    )?;
    if !enabled {
        cancel_def_children(conn, def_id)?;
    }
    Ok(changed)
}

/// Enable or disable every def whose payload matches exactly.
/// Disabled defs are skipped by `ensure_scheduled`, do not spawn follow-ups,
/// and cancel any queued children.
pub fn set_def_enabled(conn: &Connection, payload: &str, enabled: bool) -> Result<usize, JobError> {
    let changed = conn.execute(
        "UPDATE job_defs SET enabled = ?1 WHERE payload = ?2",
        params![enabled as i64, payload],
    )?;
    if !enabled {
        cancel_queued_children_for_payload(conn, payload)?;
    }
    Ok(changed)
}

fn cancel_queued_children_for_payload(conn: &Connection, payload: &str) -> Result<(), JobError> {
    conn.execute(
        "UPDATE jobs SET status = 'cancelled', finished_at = strftime('%s','now')
         WHERE status = 'queued' AND def_id IN (
             SELECT id FROM job_defs WHERE payload = ?1
         )",
        params![payload],
    )?;
    Ok(())
}

pub fn set_def_schedule(
    conn: &Connection,
    payload: &str,
    schedule: &Schedule,
) -> Result<usize, JobError> {
    conn.execute(
        "UPDATE job_defs SET schedule = ?1 WHERE payload = ?2",
        params![schedule.as_str(), payload],
    )
    .map_err(Into::into)
}

pub fn set_def_schedule_by_id(
    conn: &Connection,
    def_id: JobDefId,
    schedule: Option<&Schedule>,
) -> Result<usize, JobError> {
    let sched_str = schedule.map(|s| s.as_str());
    conn.execute(
        "UPDATE job_defs SET schedule = ?1 WHERE id = ?2",
        params![sched_str, def_id.to_string()],
    )
    .map_err(Into::into)
}

/// Cancel queued children of a def; running external work must be allowed to finish.
pub fn cancel_def_children(conn: &Connection, def_id: JobDefId) -> Result<usize, JobError> {
    conn.execute(
        "UPDATE jobs SET status = 'cancelled', finished_at = strftime('%s','now')
         WHERE def_id = ?1 AND status = 'queued'",
        params![def_id.to_string()],
    )
    .map_err(Into::into)
}

pub fn finish(
    conn: &Connection,
    id: JobId,
    now: i64,
    status: JobStatus,
    error: Option<&str>,
) -> Result<Job, JobError> {
    // 乐观锁校验：只有当前状态仍为 running 的任务才允许完成写入。
    // 如果已被 recover 判定超时重新排队或失败，则不再接受过期旧执行实例的完成写入，防止重叠覆盖。
    let changed = conn.execute(
        "UPDATE jobs SET status = ?1, finished_at = ?2, error = ?3 WHERE id = ?4 AND status = 'running'",
        params![status.as_str(), now, error, id.to_string()],
    )?;
    if changed == 0 {
        return Err(JobError::UnknownJob(id));
    }
    let job = get_job(conn, id)?.ok_or(JobError::UnknownJob(id))?;
    if matches!(
        status,
        JobStatus::Succeeded | JobStatus::Failed | JobStatus::Cancelled
    ) {
        crate::schedule::spawn_follow_up(conn, &job, now)?;
    }
    Ok(job)
}

/// Count jobs that finished (any status) after `since` (unix seconds).
pub fn count_finished_since(conn: &Connection, since: i64) -> Result<i64, JobError> {
    conn.query_row(
        "SELECT COUNT(*) FROM jobs WHERE finished_at IS NOT NULL AND finished_at > ?1",
        rusqlite::params![since],
        |row| row.get(0),
    )
    .map_err(Into::into)
}
