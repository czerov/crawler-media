use rusqlite::Connection;

use crate::types::JobError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JobChangeState {
    pub revision: i64,
    pub finished_count: i64,
}

pub fn migrate_job_change_state(conn: &Connection) -> Result<(), JobError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS job_change_state (
             id INTEGER PRIMARY KEY CHECK(id = 1),
             revision INTEGER NOT NULL DEFAULT 0,
             finished_count INTEGER NOT NULL DEFAULT 0
         );
         INSERT OR IGNORE INTO job_change_state (id, revision, finished_count)
         SELECT 1, 0, COUNT(*) FROM jobs WHERE finished_at IS NOT NULL AND finished_at > 0;

         CREATE TRIGGER IF NOT EXISTS trg_jobs_change_ins AFTER INSERT ON jobs
         BEGIN
             UPDATE job_change_state
             SET revision = revision + 1,
                 finished_count = finished_count + CASE WHEN NEW.finished_at IS NOT NULL AND NEW.finished_at > 0 THEN 1 ELSE 0 END
             WHERE id = 1;
         END;

         CREATE TRIGGER IF NOT EXISTS trg_jobs_change_upd AFTER UPDATE ON jobs
         BEGIN
             UPDATE job_change_state
             SET revision = revision + 1,
                 finished_count = finished_count
                     - CASE WHEN OLD.finished_at IS NOT NULL AND OLD.finished_at > 0 THEN 1 ELSE 0 END
                     + CASE WHEN NEW.finished_at IS NOT NULL AND NEW.finished_at > 0 THEN 1 ELSE 0 END
             WHERE id = 1;
         END;

         CREATE TRIGGER IF NOT EXISTS trg_jobs_change_del AFTER DELETE ON jobs
         BEGIN
             UPDATE job_change_state
             SET revision = revision + 1,
                 finished_count = finished_count - CASE WHEN OLD.finished_at IS NOT NULL AND OLD.finished_at > 0 THEN 1 ELSE 0 END
             WHERE id = 1;
         END;",
    )?;
    Ok(())
}

pub fn get_job_change_state(conn: &Connection) -> Result<JobChangeState, JobError> {
    conn.query_row(
        "SELECT revision, finished_count FROM job_change_state WHERE id = 1",
        [],
        |r| {
            Ok(JobChangeState {
                revision: r.get(0)?,
                finished_count: r.get(1)?,
            })
        },
    )
    .map_err(Into::into)
}
