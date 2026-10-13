use std::path::Path;

use domain::{JobDefId, JobId};
use rusqlite::Connection;

use crate::kinds::{JobKind, Schedule};
use crate::retention::RetentionBatch;
use crate::store;
use crate::types::{Job, JobDef, JobError, JobStatus};

pub struct NewDef {
    pub kind: JobKind,
    pub name: String,
    pub enabled: bool,
    pub schedule: Option<Schedule>,
    pub payload: String,
    pub timeout_secs: Option<u32>,
    pub concurrency_key: Option<String>,
}

pub trait Runner {
    fn run(&self, job: &Job) -> Result<(), String>;
}

pub struct Queue {
    conn: Connection,
}

impl Queue {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, JobError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        apply_connection_policy(&conn)?;
        store::migrate(&conn)?;
        Ok(Self { conn })
    }

    pub fn enqueue(&self, kind: JobKind, payload: &str, run_after: i64) -> Result<Job, JobError> {
        self.enqueue_with_key(kind, payload, run_after, None)
    }

    pub fn enqueue_with_key(
        &self,
        kind: JobKind,
        payload: &str,
        run_after: i64,
        concurrency_key: Option<&str>,
    ) -> Result<Job, JobError> {
        tracing::debug!(kind = kind.as_str(), run_after, concurrency_key, "任务入队");
        store::insert_job(
            &self.conn,
            None,
            kind,
            payload,
            run_after,
            concurrency_key,
            None,
        )
    }

    pub fn upsert_def(&self, def: NewDef) -> Result<JobDef, JobError> {
        store::upsert_def(&self.conn, def)
    }

    pub fn ensure_def(&self, def: NewDef) -> Result<JobDef, JobError> {
        store::ensure_def(&self.conn, def)
    }

    pub fn list_defs(&self) -> Result<Vec<JobDef>, JobError> {
        crate::schedule::list_defs(&self.conn)
    }

    pub fn def(&self, id: JobDefId) -> Result<Option<JobDef>, JobError> {
        crate::schedule::get_def(&self.conn, id)
    }

    pub fn def_name(&self, id: JobDefId) -> Result<Option<String>, JobError> {
        Ok(self.def(id)?.map(|def| def.name))
    }

    pub fn claim(&self, now: i64, limit: usize) -> Result<Vec<Job>, JobError> {
        let jobs = store::claim(&self.conn, now, limit)?;
        if !jobs.is_empty() {
            tracing::info!(count = jobs.len(), "已领取待执行任务");
        }
        Ok(jobs)
    }

    pub fn succeed(&self, id: JobId, now: i64) -> Result<Job, JobError> {
        let job = store::finish(&self.conn, id, now, JobStatus::Succeeded, None)?;
        tracing::info!(job_id = %job.id, kind = job.kind.as_str(), "任务执行成功");
        Ok(job)
    }

    /// Commit only if this is still the execution that claimed the Job.
    pub fn succeed_claimed(&self, claimed: &Job, now: i64) -> Result<Option<Job>, JobError> {
        store::finish_claimed(&self.conn, claimed, now, JobStatus::Succeeded)
    }

    pub fn fail_claimed(
        &self,
        claimed: &Job,
        now: i64,
        error: &str,
    ) -> Result<Option<Job>, JobError> {
        store::fail_claimed(&self.conn, claimed, now, error)
    }

    pub fn fail(&self, id: JobId, now: i64, error: &str) -> Result<Job, JobError> {
        store::fail(&self.conn, id, now, error)
    }

    pub fn recover(&self, now: i64) -> Result<Vec<Job>, JobError> {
        let recovered = store::recover(&self.conn, now)?;
        if !recovered.is_empty() {
            tracing::warn!(count = recovered.len(), "恢复超时任务");
        }
        Ok(recovered)
    }

    /// Recover timed-out jobs except attempts known to still be executing in
    /// this process. This preserves their running state and concurrency keys;
    /// ordinary recovery remains available after a process restart.
    pub fn recover_except(&self, now: i64, active: &[Job]) -> Result<Vec<Job>, JobError> {
        let recovered = store::recover_except(&self.conn, now, active)?;
        if !recovered.is_empty() {
            tracing::warn!(count = recovered.len(), "恢复超时任务");
        }
        Ok(recovered)
    }

    pub fn ensure_scheduled(&self, now: i64) -> Result<Vec<Job>, JobError> {
        let created = crate::schedule::ensure_scheduled(&self.conn, now)?;
        if !created.is_empty() {
            tracing::debug!(count = created.len(), "按计划生成新任务");
        }
        Ok(created)
    }

    /// 只调度/提前一个 def（按 id），不碰其他 def。见 schedule::ensure_scheduled_def。
    pub fn ensure_scheduled_for(&self, now: i64, def_id: JobDefId) -> Result<Vec<Job>, JobError> {
        let Some(def) = crate::schedule::get_def(&self.conn, def_id)? else {
            return Ok(Vec::new());
        };
        crate::schedule::ensure_scheduled_def(&self.conn, now, &def)
    }

    /// 只调度/提前 payload 匹配的 def（订阅搜索 / 目录刷新种子用）。
    pub fn ensure_scheduled_for_payload(
        &self,
        now: i64,
        payload: &str,
    ) -> Result<Vec<Job>, JobError> {
        let mut created = Vec::new();
        for def in crate::schedule::defs_for_payload(&self.conn, payload)? {
            created.extend(crate::schedule::ensure_scheduled_def(
                &self.conn, now, &def,
            )?);
        }
        Ok(created)
    }

    pub fn get(&self, id: JobId) -> Result<Option<Job>, JobError> {
        store::get_job(&self.conn, id)
    }

    /// Delete defs whose payload exactly matches, plus their child jobs.
    pub fn delete_defs_for_payload(&self, payload: &str) -> Result<usize, JobError> {
        store::delete_defs_for_payload(&self.conn, payload)
    }

    pub fn set_def_enabled_by_id(
        &self,
        def_id: JobDefId,
        enabled: bool,
    ) -> Result<usize, JobError> {
        store::set_def_enabled_by_id(&self.conn, def_id, enabled)
    }

    /// 事务内比较并更新 def 的 payload（维护配置 CAS，DB8 4.2）。
    /// 并发令牌是**保存前的原 payload**，不是 payload 内的 schema `version`。
    pub fn set_def_payload_cas(
        &self,
        def_id: JobDefId,
        expected_payload: &str,
        payload: &str,
    ) -> Result<crate::PayloadUpdate, JobError> {
        store::set_def_payload_by_id(&self.conn, def_id, expected_payload, payload)
    }

    /// Enable or disable defs whose payload matches. Disabled defs are not scheduled,
    /// and queued children are cancelled by the store layer.
    pub fn set_def_enabled(&self, payload: &str, enabled: bool) -> Result<usize, JobError> {
        store::set_def_enabled(&self.conn, payload, enabled)
    }

    pub fn set_def_schedule(&self, payload: &str, schedule: Schedule) -> Result<usize, JobError> {
        store::set_def_schedule(&self.conn, payload, &schedule)
    }

    pub fn set_def_schedule_by_id(
        &self,
        def_id: JobDefId,
        schedule: Option<Schedule>,
    ) -> Result<usize, JobError> {
        store::set_def_schedule_by_id(&self.conn, def_id, schedule.as_ref())
    }

    pub fn defs_for_payload(&self, payload: &str) -> Result<Vec<JobDef>, JobError> {
        crate::schedule::defs_for_payload(&self.conn, payload)
    }

    /// Recent child jobs (any status) whose payload matches, newest first.
    pub fn jobs_for_payload(&self, payload: &str, limit: usize) -> Vec<Job> {
        store::jobs_for_payload(&self.conn, payload, limit).unwrap_or_default()
    }

    /// Cancel queued children of a def; running external work is allowed to finish.
    pub fn cancel_def_children(&self, def_id: JobDefId) -> Result<usize, JobError> {
        store::cancel_def_children(&self.conn, def_id)
    }

    /// Count jobs finished after `since` (used by the /jobs/stream watcher).
    pub fn count_finished_since(&self, since: i64) -> Result<i64, JobError> {
        store::count_finished_since(&self.conn, since)
    }

    pub fn get_live_child(&self, def_id: JobDefId) -> Result<Option<Job>, JobError> {
        crate::schedule::get_live_child(&self.conn, def_id)
    }

    pub fn get_last_child(&self, def_id: JobDefId) -> Result<Option<Job>, JobError> {
        crate::schedule::get_last_child(&self.conn, def_id)
    }

    /// 终止态 Jobs 历史的按龄分批清理（DB8 Task 2）。每批一个事务；`dry_run` 零写入。
    pub fn prune_terminal_jobs(
        &self,
        cutoff: i64,
        batch_size: usize,
        dry_run: bool,
    ) -> Result<RetentionBatch, JobError> {
        crate::retention::prune_terminal_jobs(&self.conn, cutoff, batch_size, dry_run)
    }

    /// 批量取每个 def 的 live/last child（≤500 分块），规则与逐个查询一致。
    pub fn children_for_defs(&self, ids: &[JobDefId]) -> Result<Vec<crate::JobChildren>, JobError> {
        crate::read::children_for_defs(&self.conn, ids)
    }

    /// 只写 `progress` 列（单次执行的汇总报告 JSON）。终态持久化不覆盖该列，
    /// 因此执行结束后仍可读；不影响调度/重试语义。
    pub fn set_job_progress(&self, id: JobId, progress: &str) -> Result<(), JobError> {
        let changed = self.conn.execute(
            "UPDATE jobs SET progress = ?2 WHERE id = ?1",
            rusqlite::params![id.to_string(), progress],
        )?;
        if changed == 0 {
            return Err(JobError::UnknownJob(id));
        }
        Ok(())
    }

    pub fn tick(&self, now: i64, runner: &dyn Runner) -> Result<Vec<Job>, JobError> {
        self.recover(now)?;
        self.ensure_scheduled(now)?;
        let claimed = self.claim(now, 2)?;
        let mut finished = Vec::new();
        for job in claimed {
            let persisted = match runner.run(&job) {
                Ok(()) => self.succeed_claimed(&job, now)?,
                Err(error) => self.fail_claimed(&job, now, &error)?,
            };
            if let Some(job) = persisted {
                finished.push(job);
            }
        }
        Ok(finished)
    }
}

/// jobs 队列连接的**连接级**策略，与 Store 的四个库保持一致。
///
/// `busy_timeout`/`foreign_keys` 是连接级设置（bundled 构建默认 5000ms/ON），
/// 显式设置是为了不依赖构建默认值；`journal_mode=WAL` 是持久属性。
fn apply_connection_policy(conn: &Connection) -> Result<(), JobError> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "busy_timeout", "5000")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(())
}

#[cfg(test)]
mod policy_tests {
    use super::apply_connection_policy;

    /// 连接级 pragma 必须在**我们自己打开的连接**上断言。
    #[test]
    fn queue_connection_policy_sets_timeout_and_foreign_keys() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        apply_connection_policy(&conn).unwrap();
        let timeout: i64 = conn
            .pragma_query_value(None, "busy_timeout", |row| row.get(0))
            .unwrap();
        let foreign_keys: i64 = conn
            .pragma_query_value(None, "foreign_keys", |row| row.get(0))
            .unwrap();
        assert_eq!(timeout, 5000);
        assert_eq!(foreign_keys, 1);
    }
}
