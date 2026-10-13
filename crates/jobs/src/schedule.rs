use domain::JobDefId;
use rusqlite::{Connection, OptionalExtension, params};

use crate::rows::{JOB_COLS, map_def, map_job};
use crate::store::insert_job;
use crate::types::{Job, JobDef, JobError};

pub fn ensure_scheduled(conn: &Connection, now: i64) -> Result<Vec<Job>, JobError> {
    let mut created = Vec::new();
    for def in list_enabled_defs(conn)? {
        if def.schedule.is_none() {
            continue;
        }
        if get_live_child(conn, def.id)?.is_none() {
            created.push(insert_job(
                conn,
                Some(def.id),
                def.kind,
                &def.payload,
                now,
                def.concurrency_key.as_deref(),
                def.timeout_secs,
            )?);
        }
    }
    Ok(created)
}

/// 只调度/提前**一个** def：手动触发订阅搜索、目录刷新等场景用，绝不提前
/// 其他 def 的待执行 Job。全局调度只补缺少 live child 的 def；手动提前仅由
/// `ensure_scheduled_def` / `ensure_scheduled_for` 执行。
pub fn ensure_scheduled_def(
    conn: &Connection,
    now: i64,
    def: &JobDef,
) -> Result<Vec<Job>, JobError> {
    let mut created = Vec::new();
    if !def.enabled || (def.schedule.is_none() && now != 0) {
        return Ok(created);
    }
    // now == 0 means an explicit manual trigger for this one definition.
    if now == 0 {
        if let Some(live) = get_live_child(conn, def.id)? {
            if live.status == crate::types::JobStatus::Queued {
                conn.execute(
                    "UPDATE jobs SET run_after = 0 WHERE id = ?1",
                    params![live.id.to_string()],
                )?;
                return Ok(created);
            }
            // 维护任务正在跑时不再插第二条：当前这次用启动时的快照，下一次由周期自己排。
            // 搜索、入库等普通任务仍允许在 running 后面排至多一条立即后续。
            if def.kind.is_maintenance() {
                return Ok(created);
            }
            let queued_behind: Option<String> = conn
                .query_row(
                    "SELECT id FROM jobs WHERE def_id = ?1 AND status = 'queued' ORDER BY run_after ASC LIMIT 1",
                    params![def.id.to_string()],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(queued_id) = queued_behind {
                conn.execute(
                    "UPDATE jobs SET run_after = 0 WHERE id = ?1",
                    params![queued_id],
                )?;
                return Ok(created);
            }
            created.push(insert_job(
                conn,
                Some(def.id),
                def.kind,
                &def.payload,
                0,
                def.concurrency_key.as_deref(),
                def.timeout_secs,
            )?);
            return Ok(created);
        }
    } else if get_live_child(conn, def.id)?.is_some() {
        return Ok(created);
    }
    created.push(insert_job(
        conn,
        Some(def.id),
        def.kind,
        &def.payload,
        now,
        def.concurrency_key.as_deref(),
        def.timeout_secs,
    )?);
    Ok(created)
}

pub fn has_running_child(conn: &Connection, def_id: JobDefId) -> Result<bool, JobError> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM jobs WHERE def_id = ?1 AND status = 'running'",
        params![def_id.to_string()],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

pub fn get_live_child(conn: &Connection, def_id: JobDefId) -> Result<Option<Job>, JobError> {
    conn.query_row(
        &format!(
            "SELECT {JOB_COLS} FROM jobs
             WHERE def_id = ?1 AND status IN ('queued','running')
             ORDER BY CASE status WHEN 'queued' THEN 0 ELSE 1 END, run_after DESC LIMIT 1"
        ),
        params![def_id.to_string()],
        map_job,
    )
    .optional()
    .map_err(Into::into)
}

pub fn get_last_child(conn: &Connection, def_id: JobDefId) -> Result<Option<Job>, JobError> {
    conn.query_row(
        &format!(
            "SELECT {JOB_COLS} FROM jobs
             WHERE def_id = ?1 AND status IN ('succeeded','failed','cancelled')
             ORDER BY finished_at DESC, run_after DESC LIMIT 1"
        ),
        params![def_id.to_string()],
        map_job,
    )
    .optional()
    .map_err(Into::into)
}

pub fn spawn_follow_up(conn: &Connection, job: &Job, now: i64) -> Result<(), JobError> {
    let Some(def_id) = job.def_id else {
        return Ok(());
    };
    let Some(def) = get_def(conn, def_id)? else {
        return Ok(());
    };
    if !def.enabled {
        return Ok(());
    }
    let Some(schedule) = def.schedule else {
        return Ok(());
    };
    if get_live_child(conn, def_id)?.is_some() {
        return Ok(());
    }
    insert_job(
        conn,
        Some(def.id),
        def.kind,
        &def.payload,
        schedule.next_after(now),
        def.concurrency_key.as_deref(),
        def.timeout_secs,
    )?;
    Ok(())
}

pub fn list_defs(conn: &Connection) -> Result<Vec<JobDef>, JobError> {
    let mut stmt = conn.prepare(
        "SELECT id, kind, name, enabled, schedule, payload, timeout_secs, concurrency_key
         FROM job_defs",
    )?;
    let rows = stmt.query_map([], map_def)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

pub(crate) fn list_enabled_defs(conn: &Connection) -> Result<Vec<JobDef>, JobError> {
    let mut stmt = conn.prepare(
        "SELECT id, kind, name, enabled, schedule, payload, timeout_secs, concurrency_key
         FROM job_defs WHERE enabled = 1",
    )?;
    let rows = stmt.query_map([], map_def)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

pub fn defs_for_payload(conn: &Connection, payload: &str) -> Result<Vec<JobDef>, JobError> {
    let mut stmt = conn.prepare(
        "SELECT id, kind, name, enabled, schedule, payload, timeout_secs, concurrency_key
         FROM job_defs WHERE payload = ?1",
    )?;
    let rows = stmt.query_map(params![payload], map_def)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

pub(crate) fn get_def(conn: &Connection, id: JobDefId) -> Result<Option<JobDef>, JobError> {
    conn.query_row(
        "SELECT id, kind, name, enabled, schedule, payload, timeout_secs, concurrency_key
         FROM job_defs WHERE id = ?1",
        params![id.to_string()],
        map_def,
    )
    .optional()
    .map_err(Into::into)
}
