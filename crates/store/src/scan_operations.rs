//! Persistent ScanOperation models and methods.

use std::path::PathBuf;

use rusqlite::params;
use serde::{Deserialize, Serialize};

use super::{Store, StoreError};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScanOperationStatus {
    Queued,
    Running,
    Yielded,
    Completed,
    Partial,
    Cancelled,
    Failed,
}

impl ScanOperationStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Yielded => "yielded",
            Self::Completed => "completed",
            Self::Partial => "partial",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "queued" => Some(Self::Queued),
            "running" => Some(Self::Running),
            "yielded" => Some(Self::Yielded),
            "completed" => Some(Self::Completed),
            "partial" => Some(Self::Partial),
            "cancelled" => Some(Self::Cancelled),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }

    pub fn is_active(&self) -> bool {
        matches!(self, Self::Queued | Self::Running | Self::Yielded)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScanOperation {
    pub id: String,
    pub library_id: String,
    pub scope_signature: String,
    pub status: ScanOperationStatus,
    pub generation: i64,
    pub report: serde_json::Value,
}

impl Store {
    /// Enqueue a scan operation. If an active scan exists for this library, returns the existing operation.
    pub fn enqueue_scan(
        &self,
        library_id: &str,
        signature: &str,
        _targets: &[PathBuf],
        now_ms: i64,
    ) -> Result<ScanOperation, StoreError> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.library,
            rusqlite::TransactionBehavior::Immediate,
        )?;

        // Check if an active scan operation exists for this library
        let active_opt: Option<(String, String, String, i64, String)> = tx
            .query_row(
                "SELECT id, scope_signature, status, generation, report_json
                 FROM scan_operations
                 WHERE library_id = ?1 AND status IN ('queued', 'running', 'yielded')
                 LIMIT 1",
                [library_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?;

        if let Some((id, scope_signature, status_str, generation, report_json)) = active_opt {
            let status = ScanOperationStatus::from_str(&status_str).unwrap_or(ScanOperationStatus::Running);
            let report = serde_json::from_str(&report_json).unwrap_or_default();
            return Ok(ScanOperation {
                id,
                library_id: library_id.to_string(),
                scope_signature,
                status,
                generation,
                report,
            });
        }

        let op_id = uuid::Uuid::new_v4().to_string();
        let max_gen: Option<i64> = tx
            .query_row(
                "SELECT MAX(generation) FROM scan_operations WHERE library_id = ?1",
                [library_id],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        let generation = max_gen.unwrap_or(0) + 1;

        tx.execute(
            "INSERT INTO scan_operations
                (id, library_id, scope_signature, status, generation, created_at_ms, report_json)
             VALUES (?1, ?2, ?3, 'queued', ?4, ?5, '{}')",
            params![op_id, library_id, signature, generation, now_ms],
        )?;

        tx.commit()?;

        Ok(ScanOperation {
            id: op_id,
            library_id: library_id.to_string(),
            scope_signature: signature.to_string(),
            status: ScanOperationStatus::Queued,
            generation,
            report: serde_json::json!({}),
        })
    }

    /// Retrieve scan operation by ID.
    pub fn scan_operation(&self, id: &str) -> Result<Option<ScanOperation>, StoreError> {
        self.library
            .query_row(
                "SELECT library_id, scope_signature, status, generation, report_json
                 FROM scan_operations WHERE id = ?1",
                [id],
                |r| {
                    let library_id: String = r.get(0)?;
                    let scope_signature: String = r.get(1)?;
                    let status_str: String = r.get(2)?;
                    let generation: i64 = r.get(3)?;
                    let report_json: String = r.get(4)?;
                    let status = ScanOperationStatus::from_str(&status_str)
                        .unwrap_or(ScanOperationStatus::Failed);
                    let report = serde_json::from_str(&report_json).unwrap_or_default();
                    Ok(ScanOperation {
                        id: id.to_string(),
                        library_id,
                        scope_signature,
                        status,
                        generation,
                        report,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// Record paths seen during an operation chunk within an Immediate transaction.
    pub fn record_scan_seen(&self, operation_id: &str, paths: &[PathBuf]) -> Result<(), StoreError> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.library,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        for p in paths {
            tx.execute(
                "INSERT OR IGNORE INTO scan_seen (operation_id, path) VALUES (?1, ?2)",
                params![operation_id, p.display().to_string()],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Check if path has been seen by this operation.
    pub fn is_scan_seen(&self, operation_id: &str, path: &str) -> Result<bool, StoreError> {
        let found: bool = self.library.query_row(
            "SELECT EXISTS(SELECT 1 FROM scan_seen WHERE operation_id = ?1 AND path = ?2)",
            params![operation_id, path],
            |r| r.get(0),
        )?;
        Ok(found)
    }

    /// Clear seen entries when operation completes or is cancelled.
    pub fn clear_scan_seen(&self, operation_id: &str) -> Result<(), StoreError> {
        self.library.execute(
            "DELETE FROM scan_seen WHERE operation_id = ?1",
            [operation_id],
        )?;
        Ok(())
    }
    /// Cancel a scan operation if it is active.
    pub fn cancel_scan(&self, id: &str, _now_ms: i64) -> Result<bool, StoreError> {
        let changed = self.library.execute(
            "UPDATE scan_operations SET status = 'cancelled'
             WHERE id = ?1 AND status IN ('queued', 'running', 'yielded')",
            [id],
        )?;
        Ok(changed > 0)
    }

    /// Update operation status and report. Does not overwrite terminal statuses (cancelled/completed/failed).
    pub fn update_scan_operation_status(
        &self,
        id: &str,
        status: ScanOperationStatus,
        report: &serde_json::Value,
    ) -> Result<bool, StoreError> {
        let changed = self.library.execute(
            "UPDATE scan_operations SET status = ?1, report_json = ?2
             WHERE id = ?3 AND status NOT IN ('cancelled', 'completed', 'failed')",
            params![status.as_str(), report.to_string(), id],
        )?;
        Ok(changed > 0)
    }

    /// Atomically claim the oldest queued scan operation, setting its status to running.
    ///
    /// 用 `BEGIN IMMEDIATE`：SELECT 与 UPDATE 必须在同一个写事务里，否则多连接下
    /// 两个 worker 可以同时读到同一行 queued（B3 §6）。
    pub fn claim_next_queued_scan(&self, excluding: &[String]) -> Result<Option<ScanOperation>, StoreError> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.library,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let mut query = "SELECT id, library_id, scope_signature, status, generation, report_json
                         FROM scan_operations
                         WHERE status = 'queued'".to_string();
        if !excluding.is_empty() {
            let placeholders: Vec<String> = (1..=excluding.len()).map(|i| format!("?{}", i)).collect();
            query.push_str(&format!(" AND id NOT IN ({})", placeholders.join(",")));
        }
        query.push_str(" ORDER BY created_at_ms ASC, rowid ASC LIMIT 1");

        let mut stmt = tx.prepare(&query)?;
        let mut params: Vec<&dyn rusqlite::ToSql> = Vec::new();
        for id in excluding {
            params.push(id);
        }
        let op_opt = stmt
            .query_row(rusqlite::params_from_iter(params), |r| {
                let id: String = r.get(0)?;
                let library_id: String = r.get(1)?;
                let scope_signature: String = r.get(2)?;
                let status_str: String = r.get(3)?;
                let generation: i64 = r.get(4)?;
                let report_json: String = r.get(5)?;
                let status = ScanOperationStatus::from_str(&status_str)
                    .unwrap_or(ScanOperationStatus::Queued);
                let report = serde_json::from_str(&report_json).unwrap_or_default();
                Ok(ScanOperation {
                    id,
                    library_id,
                    scope_signature,
                    status,
                    generation,
                    report,
                })
            })
            .optional()?;
        drop(stmt);

        if let Some(mut op) = op_opt {
            let updated = tx.execute(
                "UPDATE scan_operations SET status = 'running' WHERE id = ?1 AND status = 'queued'",
                params![op.id],
            )?;
            if updated > 0 {
                tx.commit()?;
                op.status = ScanOperationStatus::Running;
                return Ok(Some(op));
            }
        }
        Ok(None)
    }

    /// Retrieve the oldest queued scan operation without claiming it.
    pub fn next_queued_scan(&self) -> Result<Option<ScanOperation>, StoreError> {
        self.library
            .query_row(
                "SELECT id, library_id, scope_signature, status, generation, report_json
                 FROM scan_operations
                 WHERE status = 'queued'
                 ORDER BY created_at_ms ASC, rowid ASC
                 LIMIT 1",
                [],
                |r| {
                    let id: String = r.get(0)?;
                    let library_id: String = r.get(1)?;
                    let scope_signature: String = r.get(2)?;
                    let status_str: String = r.get(3)?;
                    let generation: i64 = r.get(4)?;
                    let report_json: String = r.get(5)?;
                    let status = ScanOperationStatus::from_str(&status_str)
                        .unwrap_or(ScanOperationStatus::Queued);
                    let report = serde_json::from_str(&report_json).unwrap_or_default();
                    Ok(ScanOperation {
                        id,
                        library_id,
                        scope_signature,
                        status,
                        generation,
                        report,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// Recover orphaned running scans on startup: validates if the library still accepts work;
    /// if deleted/deleting, marks cancelled, otherwise re-queues.
    pub fn recover_orphaned_scans(&self) -> Result<usize, StoreError> {
        let running: Vec<(String, String)> = {
            let mut stmt = self.library.prepare(
                "SELECT id, library_id FROM scan_operations WHERE status = 'running'",
            )?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            let mut list = Vec::new();
            for r in rows {
                list.push(r?);
            }
            list
        };

        let mut recovered = 0;
        for (op_id, lib_id) in running {
            let accepts = match self.library_accepts_work(&lib_id) {
                Ok(accepts) => accepts,
                Err(error) => {
                    // 读不到冻结状态时保守地当作不可工作（cancel），但必须留痕。
                    tracing::error!(%error, library_id = %lib_id, %op_id, "恢复扫描时读取 Library 工作状态失败，按不可工作处理");
                    false
                }
            };
            if accepts {
                let updated = self.library.execute(
                    "UPDATE scan_operations SET status = 'queued' WHERE id = ?1 AND status = 'running'",
                    params![op_id],
                )?;
                recovered += updated;
            } else if let Err(error) = self.library.execute(
                "UPDATE scan_operations SET status = 'cancelled' WHERE id = ?1 AND status = 'running'",
                params![op_id],
            ) {
                tracing::error!(%error, library_id = %lib_id, %op_id, "恢复扫描时取消失效 operation 失败");
            }
        }
        Ok(recovered)
    }
}

trait RusqliteOptionalExt<T> {
    fn optional(self) -> Result<Option<T>, rusqlite::Error>;
}

impl<T> RusqliteOptionalExt<T> for Result<T, rusqlite::Error> {
    fn optional(self) -> Result<Option<T>, rusqlite::Error> {
        match self {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }
}
