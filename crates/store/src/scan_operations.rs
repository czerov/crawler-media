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

/// 扫描意图：`full` 表示走完整库根，`targeted` 表示只走 `targets`。
///
/// 之所以要把它和 `targets` 分开持久化：空 `targets` 既可能表示「整库」，
/// 也可能表示「本轮没有新增目标」，只靠数组无法区分（P2-1）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum ScanScopeKind {
    Full,
    Targeted,
}

impl ScanScopeKind {
    fn from_targets(targets: &[PathBuf]) -> Self {
        if targets.is_empty() {
            Self::Full
        } else {
            Self::Targeted
        }
    }

    fn as_str(&self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Targeted => "targeted",
        }
    }

    fn from_str(raw: &str) -> Option<Self> {
        match raw {
            "full" => Some(Self::Full),
            "targeted" => Some(Self::Targeted),
            _ => None,
        }
    }

    /// 合并规则：Full 支配 Targeted（整库请求永远不会被增量目标缩小）。
    fn merge(self, incoming: Self) -> Self {
        if self == Self::Full || incoming == Self::Full {
            Self::Full
        } else {
            Self::Targeted
        }
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
    /// 增量扫描目标（`scope_kind == targeted` 时生效）。
    pub targets: Vec<PathBuf>,
    scope_kind: ScanScopeKind,
    /// 每次并入请求（含重复 target）都自增；worker 据此判断是否需要补扫一轮。
    pub dirty_rev: i64,
    /// worker 已消费到的 `dirty_rev`。
    pub consumed_rev: i64,
}

impl ScanOperation {
    /// 是否整库扫描（`scope_kind == full`）。
    pub fn is_full_scope(&self) -> bool {
        self.scope_kind == ScanScopeKind::Full
    }

    /// 持久化的扫描意图字符串（`full` / `targeted`）。
    pub fn scope_kind_str(&self) -> &'static str {
        self.scope_kind.as_str()
    }
}

fn parse_targets(raw: &str) -> Vec<PathBuf> {
    serde_json::from_str(raw).unwrap_or_default()
}

fn targets_json(targets: &[PathBuf]) -> String {
    serde_json::to_string(targets).unwrap_or_else(|_| "[]".to_string())
}

/// 列顺序固定为：id, library_id, scope_signature, status, generation, report_json,
/// targets_json, scope_kind, dirty_rev, consumed_rev。
fn map_scan_operation(row: &rusqlite::Row<'_>) -> rusqlite::Result<ScanOperation> {
    let status_str: String = row.get(3)?;
    let report_json: String = row.get(5)?;
    let targets_raw: String = row.get(6)?;
    let kind_raw: String = row.get(7)?;
    Ok(ScanOperation {
        id: row.get(0)?,
        library_id: row.get(1)?,
        scope_signature: row.get(2)?,
        status: ScanOperationStatus::from_str(&status_str)
            .unwrap_or(ScanOperationStatus::Queued),
        generation: row.get(4)?,
        report: serde_json::from_str(&report_json).unwrap_or_default(),
        targets: parse_targets(&targets_raw),
        // 老库没有 scope_kind：默认 full 是安全方向（整库多扫，绝不缩小）。
        scope_kind: ScanScopeKind::from_str(&kind_raw).unwrap_or(ScanScopeKind::Full),
        dirty_rev: row.get(8)?,
        consumed_rev: row.get(9)?,
    })
}

/// 所有 `ScanOperation` 查询共用的列清单。
const SCAN_OPERATION_COLUMNS: &str =
    "id, library_id, scope_signature, status, generation, report_json, targets_json, \
     scope_kind, dirty_rev, consumed_rev";

impl Store {
    /// Enqueue a scan operation. If an active scan exists for this library, returns the existing operation.
    ///
    /// `targets` 非空 = 只扫描这些子目录（`targeted`），空 = 整库（`full`）。命中已有 active
    /// operation 时按「Full 支配 Targeted」合并意图，并把新目标并进去（持久化到行上）；
    /// 每次并入（含重复 target）都自增 `dirty_rev`，worker 据此补扫一轮，重复事件不会被吞。
    pub fn enqueue_scan(
        &self,
        library_id: &str,
        signature: &str,
        targets: &[PathBuf],
        now_ms: i64,
    ) -> Result<ScanOperation, StoreError> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.library,
            rusqlite::TransactionBehavior::Immediate,
        )?;

        let incoming_kind = ScanScopeKind::from_targets(targets);

        // Check if an active scan operation exists for this library
        let active_opt: Option<(String, String, String, String, i64, String, String, String, i64, i64)> = tx
            .query_row(
                &format!(
                    "SELECT {SCAN_OPERATION_COLUMNS}
                     FROM scan_operations
                     WHERE library_id = ?1 AND status IN ('queued', 'running', 'yielded')
                     LIMIT 1"
                ),
                [library_id],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                        r.get(7)?,
                        r.get(8)?,
                        r.get(9)?,
                    ))
                },
            )
            .optional()?;

        if let Some((
            id,
            _library_id,
            scope_signature,
            status_str,
            generation,
            report_json,
            targets_raw,
            kind_raw,
            dirty_rev,
            consumed_rev,
        )) = active_opt
        {
            let status = ScanOperationStatus::from_str(&status_str).unwrap_or(ScanOperationStatus::Running);
            let report = serde_json::from_str(&report_json).unwrap_or_default();
            let existing_kind = ScanScopeKind::from_str(&kind_raw).unwrap_or(ScanScopeKind::Full);
            let merged_kind = existing_kind.merge(incoming_kind);
            // Full 支配 Targeted：整库语义下 target 列表没有意义，清空以免被误读为缩小范围。
            let mut merged = if merged_kind == ScanScopeKind::Full {
                Vec::new()
            } else {
                parse_targets(&targets_raw)
            };
            let mut added = 0usize;
            if merged_kind == ScanScopeKind::Targeted {
                for target in targets {
                    if !merged.contains(target) {
                        merged.push(target.clone());
                        added += 1;
                    }
                }
            }
            // 即使 target 重复、即使意图被 Full 吸收，也必须自增版本：
            // 这是「同一 target 的后续事件不被吞掉」的唯一依据。
            let next_dirty = dirty_rev + 1;
            tx.execute(
                "UPDATE scan_operations
                 SET targets_json = ?1, scope_kind = ?2, dirty_rev = ?3
                 WHERE id = ?4",
                params![targets_json(&merged), merged_kind.as_str(), next_dirty, id],
            )?;
            tracing::info!(
                %library_id,
                op_id = %id,
                added,
                scope_kind = merged_kind.as_str(),
                dirty_rev = next_dirty,
                "扫描请求已并入进行中的扫描操作"
            );
            tx.commit()?;
            return Ok(ScanOperation {
                id,
                library_id: library_id.to_string(),
                scope_signature,
                status,
                generation,
                report,
                targets: merged,
                scope_kind: merged_kind,
                dirty_rev: next_dirty,
                consumed_rev,
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
                (id, library_id, scope_signature, status, generation, created_at_ms, report_json,
                 targets_json, scope_kind, dirty_rev, consumed_rev)
             VALUES (?1, ?2, ?3, 'queued', ?4, ?5, '{}', ?6, ?7, 0, 0)",
            params![
                op_id,
                library_id,
                signature,
                generation,
                now_ms,
                targets_json(targets),
                incoming_kind.as_str()
            ],
        )?;

        tx.commit()?;

        Ok(ScanOperation {
            id: op_id,
            library_id: library_id.to_string(),
            scope_signature: signature.to_string(),
            status: ScanOperationStatus::Queued,
            generation,
            report: serde_json::json!({}),
            targets: targets.to_vec(),
            scope_kind: incoming_kind,
            // 新建 operation 尚未执行，不需要补扫版本；后续并入请求才会自增。
            dirty_rev: 0,
            consumed_rev: 0,
        })
    }

    /// Retrieve scan operation by ID.
    pub fn scan_operation(&self, id: &str) -> Result<Option<ScanOperation>, StoreError> {
        self.library
            .query_row(
                &format!("SELECT {SCAN_OPERATION_COLUMNS} FROM scan_operations WHERE id = ?1"),
                [id],
                map_scan_operation,
            )
            .optional()
            .map_err(Into::into)
    }

    /// 某个 Library 的扫描 operation，最新在前（排查与测试用）。
    pub fn scan_operations_for_library(
        &self,
        library_id: &str,
    ) -> Result<Vec<ScanOperation>, StoreError> {
        let mut stmt = self.library.prepare(&format!(
            "SELECT {SCAN_OPERATION_COLUMNS}
             FROM scan_operations WHERE library_id = ?1
             ORDER BY created_at_ms DESC, rowid DESC"
        ))?;
        let rows = stmt.query_map([library_id], map_scan_operation)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// 推进已消费的 `dirty_rev`：只前进不后退，保证单调。
    pub fn advance_scan_consumed_rev(&self, id: &str, rev: i64) -> Result<bool, StoreError> {
        let changed = self.library.execute(
            "UPDATE scan_operations SET consumed_rev = ?1 WHERE id = ?2 AND consumed_rev < ?1",
            params![rev, id],
        )?;
        Ok(changed > 0)
    }

    /// Record paths seen during an operation chunk within an Immediate transaction.
    ///
    /// 同时记录文件版本（mtime_ms/file_size），恢复时据此复核「文件是否变化」。
    pub fn record_scan_seen(&self, operation_id: &str, paths: &[PathBuf]) -> Result<(), StoreError> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.library,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        for p in paths {
            let (mtime_ms, file_size) = file_version(p);
            tx.execute(
                "INSERT INTO scan_seen (operation_id, path, mtime_ms, file_size)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(operation_id, path)
                 DO UPDATE SET mtime_ms = excluded.mtime_ms, file_size = excluded.file_size",
                params![operation_id, p.display().to_string(), mtime_ms, file_size],
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

    /// 断点命中复核：只有「台账行仍存在、未标记 missing、文件版本未变化」时才可以跳过。
    ///
    /// 恢复的文件（missing_at 已清除/曾标记）与内容变化的文件必须重新处理（P2-3）。
    pub fn scan_seen_is_current(&self, operation_id: &str, path: &str) -> Result<bool, StoreError> {
        let seen: Option<(i64, i64)> = self
            .library
            .query_row(
                "SELECT mtime_ms, file_size FROM scan_seen WHERE operation_id = ?1 AND path = ?2",
                params![operation_id, path],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((seen_mtime, seen_size)) = seen else {
            return Ok(false);
        };
        // 台账行必须还在，且没有被标记为 missing：文件被删过又恢复时，
        // 单看路径会误判「已处理」，导致 missing 永远清不掉、文件也不重新入账。
        let missing: Option<Option<i64>> = self
            .library
            .query_row(
                "SELECT missing_at FROM ledger WHERE path = ?1 LIMIT 1",
                params![path],
                |r| r.get(0),
            )
            .optional()?;
        match missing {
            None | Some(Some(_)) => return Ok(false),
            Some(None) => {}
        }
        // 0/0 视为「版本未知」（老断点行）：只做台账复核，保持向后兼容。
        if seen_mtime == 0 && seen_size == 0 {
            return Ok(true);
        }
        let (mtime_ms, file_size) = file_version(std::path::Path::new(path));
        Ok(mtime_ms == seen_mtime && file_size == seen_size)
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
        let mut query = format!(
            "SELECT {SCAN_OPERATION_COLUMNS}
             FROM scan_operations
             WHERE status = 'queued'"
        );
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
            .query_row(rusqlite::params_from_iter(params), map_scan_operation)
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

/// 文件版本：mtime 毫秒 + 字节数；读不到时返回 0/0（视为「版本未知」）。
fn file_version(path: &std::path::Path) -> (i64, i64) {
    match std::fs::metadata(path) {
        Ok(meta) => {
            let mtime_ms = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            (mtime_ms, meta.len() as i64)
        }
        Err(_) => (0, 0),
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
