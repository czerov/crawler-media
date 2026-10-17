//! Durable chapter scene-frame obligations.
//!
//! One row is one (ledger, source version, chapter target set) obligation. The
//! request is persisted before any worker starts, so a crash leaves `queued`
//! work recoverable instead of losing it with the in-memory queue.

use domain::LedgerId;
use rusqlite::{params, OptionalExtension};
use std::str::FromStr;

use super::{Store, StoreError};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChapterFrameTask {
    pub id: String,
    pub ledger_id: LedgerId,
    pub source_version: String,
    pub targets: Vec<(i64, i64)>,
    pub targets_hash: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChapterFrameTaskCounts {
    pub queued: usize,
    pub running: usize,
    pub completed: usize,
    pub failed: usize,
}

impl ChapterFrameTaskCounts {
    /// Terminal rows (`completed` + `failed`).
    pub fn terminal(&self) -> usize {
        self.completed + self.failed
    }
}

impl Store {
    /// Persist one obligation. `false` means the same
    /// `(ledger_id, source_version, targets_hash)` already exists, so the
    /// caller must not start the work twice.
    pub fn enqueue_chapter_frame_task(
        &self,
        task: &ChapterFrameTask,
    ) -> Result<bool, StoreError> {
        let targets_json = serde_json::to_string(&task.targets)?;
        let changed = self.library.execute(
            "INSERT INTO chapter_frame_tasks (
                 id, ledger_id, source_version, targets_json, targets_hash,
                 status, created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, 'queued', ?6)
             ON CONFLICT(ledger_id, source_version, targets_hash) DO NOTHING",
            params![
                task.id,
                compact_ledger_id(task.ledger_id),
                task.source_version,
                targets_json,
                task.targets_hash,
                super::unix_now() * 1000,
            ],
        )?;
        Ok(changed > 0)
    }

    /// Queued obligations in dispatch order. Only `queued` rows are returned;
    /// `running` rows belong to an in-memory worker window.
    pub fn pending_chapter_frame_tasks(
        &self,
        limit: usize,
    ) -> Result<Vec<ChapterFrameTask>, StoreError> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let mut statement = self.library.prepare(
            "SELECT id, ledger_id, source_version, targets_json, targets_hash
             FROM chapter_frame_tasks
             WHERE status = 'queued'
             ORDER BY created_at_ms, id
             LIMIT ?1",
        )?;
        let rows = statement.query_map([limit], map_task)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// CAS claim `queued -> running`. `false` means another dispatcher (or a
    /// recovered row) already owns it.
    pub fn claim_chapter_frame_task(&self, id: &str) -> Result<bool, StoreError> {
        let changed = self.library.execute(
            "UPDATE chapter_frame_tasks
             SET status = 'running', started_at_ms = ?2
             WHERE id = ?1 AND status = 'queued'",
            params![id, super::unix_now() * 1000],
        )?;
        Ok(changed > 0)
    }

    /// Acknowledge a running obligation. Terminal rows stay in the table so
    /// operators and tests can observe what actually finished.
    pub fn finish_chapter_frame_task(
        &self,
        id: &str,
        succeeded: bool,
        error: Option<&str>,
    ) -> Result<(), StoreError> {
        let status = if succeeded { "completed" } else { "failed" };
        self.library.execute(
            "UPDATE chapter_frame_tasks
             SET status = ?2, error = ?3, finished_at_ms = ?4
             WHERE id = ?1 AND status = 'running'",
            params![id, status, error, super::unix_now() * 1000],
        )?;
        Ok(())
    }

    /// Startup recovery: every `running` row belongs to a process that is
    /// gone, so it becomes dispatchable again. Returns the number recovered.
    pub fn recover_chapter_frame_tasks(&self) -> Result<usize, StoreError> {
        let changed = self.library.execute(
            "UPDATE chapter_frame_tasks
             SET status = 'queued', started_at_ms = NULL
             WHERE status = 'running'",
            [],
        )?;
        Ok(changed)
    }

    pub fn chapter_frame_task_counts(&self) -> Result<ChapterFrameTaskCounts, StoreError> {
        let mut counts = ChapterFrameTaskCounts::default();
        let mut statement = self
            .library
            .prepare("SELECT status, COUNT(*) FROM chapter_frame_tasks GROUP BY status")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (status, count) = row?;
            let count = usize::try_from(count).unwrap_or(usize::MAX);
            match status.as_str() {
                "queued" => counts.queued = count,
                "running" => counts.running = count,
                "completed" => counts.completed = count,
                "failed" => counts.failed = count,
                _ => {}
            }
        }
        Ok(counts)
    }

    pub fn chapter_frame_task(&self, id: &str) -> Result<Option<ChapterFrameTask>, StoreError> {
        let task = self
            .library
            .query_row(
                "SELECT id, ledger_id, source_version, targets_json, targets_hash
                 FROM chapter_frame_tasks WHERE id = ?1",
                [id],
                map_task,
            )
            .optional()?;
        Ok(task)
    }
}

fn map_task(row: &rusqlite::Row<'_>) -> rusqlite::Result<ChapterFrameTask> {
    let ledger: String = row.get(1)?;
    let targets_json: String = row.get(3)?;
    let targets = serde_json::from_str::<Vec<(i64, i64)>>(&targets_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            targets_json.len(),
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })?;
    let ledger_id = LedgerId::from_str(&ledger).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            ledger.len(),
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })?;
    Ok(ChapterFrameTask {
        id: row.get(0)?,
        ledger_id,
        source_version: row.get(2)?,
        targets,
        targets_hash: row.get(4)?,
    })
}

fn compact_ledger_id(ledger_id: LedgerId) -> String {
    ledger_id.to_string().replace('-', "")
}
