//! Persistent ScanOperation models and methods.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

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
        // 三个职责共用同一个 IMMEDIATE 事务边界：读活动 operation → 合并意图或新建。
        let op = match active_scan_operation(&tx, library_id)? {
            Some(existing) => merge_scan_intent(&tx, existing, targets)?,
            None => create_scan_operation(&tx, library_id, signature, targets, now_ms)?,
        };
        tx.commit()?;
        Ok(op)
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

    /// 采集文件版本事实：`(mtime_ms, file_size, content_digest)`。
    ///
    /// **唯一会触碰文件系统的地方**，必须在 Store 锁外调用（P2-NEW-2）。
    /// 普通文件用 size + 毫秒 mtime；`.strm` 只有 URL 文本，同 size/同 mtime 的替换
    /// 无法靠元数据识别，因此额外记录受限内容指纹（P2-3）。
    pub fn scan_seen_stamp(path: &Path) -> (i64, i64, Option<String>) {
        let (mtime_ms, file_size) = file_version(path);
        let content_digest = path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("strm"))
            .then(|| bounded_content_digest(path))
            .flatten();
        (mtime_ms, file_size, content_digest)
    }

    /// 已记录版本是否仍能证明当前版本是同一文件。
    ///
    /// 有内容摘要时以「大小 + 摘要」定身份（STRM 同步改 mtime 不算变化）；否则退化为
    /// size + 毫秒 mtime。0/0 且无摘要 = 版本未知，绝不视为同一版本。
    pub fn scan_seen_stamp_matches(
        recorded: &(i64, i64, Option<String>),
        current: &(i64, i64, Option<String>),
    ) -> bool {
        if recorded.0 == 0 && recorded.1 == 0 && recorded.2.is_none() {
            return false;
        }
        if recorded.1 != current.1 {
            return false;
        }
        match (&recorded.2, &current.2) {
            (Some(recorded), Some(observed)) => recorded == observed,
            (None, None) => recorded.0 == current.0,
            // 一侧有摘要、另一侧没有：无法证明是同一版本，按变化处理。
            _ => false,
        }
    }

    /// 批量写断点版本事实：**纯 DB，不做文件 IO**（P2-NEW-2）。
    ///
    /// 调用方必须先在 Store 锁外用 [`Store::scan_seen_stamp`] 采集版本，再进锁写库。
    pub fn record_scan_seen_stamps(
        &self,
        operation_id: &str,
        stamps: &[(String, i64, i64, Option<String>)],
    ) -> Result<(), StoreError> {
        if stamps.is_empty() {
            return Ok(());
        }
        let tx = rusqlite::Transaction::new_unchecked(
            &self.library,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        for (path, mtime_ms, file_size, content_digest) in stamps {
            tx.execute(
                "INSERT INTO scan_seen (operation_id, path, mtime_ms, file_size, content_digest)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(operation_id, path)
                 DO UPDATE SET mtime_ms = excluded.mtime_ms, file_size = excluded.file_size,
                               content_digest = excluded.content_digest",
                params![operation_id, path, mtime_ms, file_size, content_digest],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// 批量读断点事实：**纯 DB，不做文件 IO**（P2-NEW-2）。
    ///
    /// 返回 `path -> (mtime_ms, file_size, content_digest, ledger_current)`；
    /// map 里缺失的路径即「本轮新文件」。
    pub fn scan_seen_checkpoints(
        &self,
        operation_id: &str,
        paths: &[String],
    ) -> Result<HashMap<String, (i64, i64, Option<String>, bool)>, StoreError> {
        let mut checkpoints = HashMap::new();
        if paths.is_empty() {
            return Ok(checkpoints);
        }
        let placeholders: Vec<String> = (2..=paths.len() + 1).map(|i| format!("?{i}")).collect();
        let sql = format!(
            "SELECT s.path, s.mtime_ms, s.file_size, s.content_digest,
                    EXISTS(SELECT 1 FROM ledger l WHERE l.path = s.path) AS ledger_exists,
                    (SELECT l.missing_at FROM ledger l WHERE l.path = s.path LIMIT 1) AS missing_at
             FROM scan_seen s
             WHERE s.operation_id = ?1 AND s.path IN ({})",
            placeholders.join(",")
        );
        let mut stmt = self.library.prepare(&sql)?;
        let mut params: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(paths.len() + 1);
        params.push(&operation_id);
        for path in paths {
            params.push(path);
        }
        let rows = stmt.query_map(rusqlite::params_from_iter(params), |row| {
            let path: String = row.get(0)?;
            let ledger_exists: bool = row.get(4)?;
            let missing_at: Option<i64> = row.get(5)?;
            Ok((
                path,
                (
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    ledger_exists && missing_at.is_none(),
                ),
            ))
        })?;
        for row in rows {
            let (path, checkpoint) = row?;
            checkpoints.insert(path, checkpoint);
        }
        Ok(checkpoints)
    }

    /// Record paths seen during an operation chunk (convenience: 自行采集版本事实).
    ///
    /// 生产 runner 不走这里：它必须先在 Store 锁外采集版本，再调用
    /// [`Self::record_scan_seen_stamps`]（P2-NEW-2）。
    pub fn record_scan_seen(&self, operation_id: &str, paths: &[PathBuf]) -> Result<(), StoreError> {
        let stamps: Vec<(String, i64, i64, Option<String>)> = paths
            .iter()
            .map(|path| {
                let (mtime_ms, file_size, digest) = Self::scan_seen_stamp(path);
                (path.display().to_string(), mtime_ms, file_size, digest)
            })
            .collect();
        self.record_scan_seen_stamps(operation_id, &stamps)
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

    /// 断点命中复核：只有「台账行仍存在、未标记 missing、文件版本仍能证明同一版本」才可跳过。
    ///
    /// 恢复的文件（missing_at 已清除/曾标记）与内容变化的文件必须重新处理（P2-3）。
    /// 版本未知（0/0 且无内容摘要，例如老断点行）绝不视为 current。
    pub fn scan_seen_is_current(&self, operation_id: &str, path: &str) -> Result<bool, StoreError> {
        let current = Self::scan_seen_stamp(Path::new(path));
        let checkpoints = self.scan_seen_checkpoints(operation_id, &[path.to_string()])?;
        Ok(checkpoints.get(path).is_some_and(
            |(mtime_ms, file_size, digest, ledger_current)| {
                *ledger_current
                    && Self::scan_seen_stamp_matches(
                        &(*mtime_ms, *file_size, digest.clone()),
                        &current,
                    )
            },
        ))
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

    /// 只更新状态、**不动 `report_json`**（P3-NEW-1）。
    ///
    /// 补扫重排的事务刚把跨轮累计报告写进行里；worker 进入 `running` 时若用
    /// [`Self::update_scan_operation_status`] 传空报告，会把这份已提交前缀抹掉，
    /// 崩溃后只剩空报告。终态行（cancelled/completed/failed）不被覆盖。
    pub fn set_scan_operation_status(
        &self,
        id: &str,
        status: ScanOperationStatus,
    ) -> Result<bool, StoreError> {
        let changed = self.library.execute(
            "UPDATE scan_operations SET status = ?1
             WHERE id = ?2 AND status NOT IN ('cancelled', 'completed', 'failed')",
            params![status.as_str(), id],
        )?;
        Ok(changed > 0)
    }

    /// 原子升级持久化的 `scope_signature`（P2-NEW-1）。
    ///
    /// 旧构建写下的签名（`kind:count:root1,root2,...`）在新编码下会被误判为配置变化；
    /// 语义一致时用 CAS 就地把行升级成新编码，而不是取消 operation。只有当前值仍等于
    /// `expected` 才写入，因此与并发配置变更不会互相覆盖。
    pub fn upgrade_scan_scope_signature(
        &self,
        id: &str,
        expected: &str,
        upgraded: &str,
    ) -> Result<bool, StoreError> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.library,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let changed = tx.execute(
            "UPDATE scan_operations SET scope_signature = ?1
             WHERE id = ?2 AND scope_signature = ?3",
            params![upgraded, id, expected],
        )?;
        tx.commit()?;
        Ok(changed > 0)
    }

    /// 终态 CAS：只有 `dirty_rev` 仍等于本轮开始时消费的版本才写终态；否则补扫重排（P3-1）。
    ///
    /// 判定与写入在同一个 IMMEDIATE 事务里完成：另一连接在 worker 判定之后、写终态之前
    /// 并入的事件不会被终态 UPDATE 覆盖。
    ///
    /// 返回 `Some(consumed_rev)` = 期间有新事件，已推进版本并回到 `queued`（调用方不得写终态）；
    /// `None` = 已写终态，或 operation 已不存在/已是终态（未覆盖）。
    pub fn finalize_or_requeue_scan(
        &self,
        id: &str,
        expected_dirty_rev: i64,
        terminal_status: ScanOperationStatus,
        report: &serde_json::Value,
    ) -> Result<Option<i64>, StoreError> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.library,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let current: Option<(String, i64, i64)> = tx
            .query_row(
                "SELECT status, dirty_rev, consumed_rev FROM scan_operations WHERE id = ?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let Some((status_raw, dirty_rev, consumed_rev)) = current else {
            return Ok(None);
        };
        let status = ScanOperationStatus::from_str(&status_raw).unwrap_or(ScanOperationStatus::Queued);
        if !status.is_active() {
            return Ok(None);
        }
        if dirty_rev != expected_dirty_rev {
            let consumed = consumed_rev.max(dirty_rev);
            tx.execute(
                "UPDATE scan_operations
                 SET status = 'queued', consumed_rev = ?1, report_json = ?2
                 WHERE id = ?3 AND status IN ('queued', 'running', 'yielded')",
                params![consumed, report.to_string(), id],
            )?;
            tx.commit()?;
            return Ok(Some(consumed));
        }
        let changed = tx.execute(
            "UPDATE scan_operations SET status = ?1, report_json = ?2
             WHERE id = ?3 AND status IN ('queued', 'running', 'yielded') AND dirty_rev = ?4",
            params![
                terminal_status.as_str(),
                report.to_string(),
                id,
                expected_dirty_rev
            ],
        )?;
        if changed > 0 {
            tx.execute("DELETE FROM scan_seen WHERE operation_id = ?1", [id])?;
            tx.commit()?;
            return Ok(None);
        }
        tx.commit()?;
        Ok(None)
    }

    /// 给已存在的台账行重新挂上扫描 probe 义务（P2-3：文件内容变化后派生事实失效）。
    ///
    /// 义务表是 durable 合同：dispatcher 会在后续轮次按现有 probe 策略重排探测。
    pub fn arm_scan_probe_obligation(
        &self,
        ledger_id: &str,
        now_ms: i64,
        reason: &str,
    ) -> Result<(), StoreError> {
        self.library.execute(
            "INSERT INTO ledger_probe_obligations (ledger_id, created_at_ms, reason)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(ledger_id) DO UPDATE SET
                 created_at_ms = excluded.created_at_ms, reason = excluded.reason",
            params![ledger_id, now_ms, reason],
        )?;
        Ok(())
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

/// `.strm` 内容指纹的读取上限（与 B3 的 64KiB decoded 上限一致）。
const STRM_DIGEST_LIMIT: usize = 64 * 1024;

/// 前 64KiB 的 FNV-1a 64 指纹：只用于「文件是否变化」的判定，不是安全摘要。
fn bounded_content_digest(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).ok()?;
    let mut buffer = [0u8; STRM_DIGEST_LIMIT];
    let mut filled = 0usize;
    while filled < buffer.len() {
        match file.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return None,
        }
    }
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in &buffer[..filled] {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    Some(format!("{filled:08x}:{hash:016x}"))
}

/// 读取某 Library 当前的 active scan operation（同一 IMMEDIATE 事务内）。
fn active_scan_operation(
    tx: &rusqlite::Transaction<'_>,
    library_id: &str,
) -> Result<Option<ScanOperation>, StoreError> {
    tx.query_row(
        &format!(
            "SELECT {SCAN_OPERATION_COLUMNS}
             FROM scan_operations
             WHERE library_id = ?1 AND status IN ('queued', 'running', 'yielded')
             LIMIT 1"
        ),
        [library_id],
        map_scan_operation,
    )
    .optional()
    .map_err(Into::into)
}

/// 把新意图并入已有 active operation：Full 支配 Targeted，重复 target 也推进 `dirty_rev`。
fn merge_scan_intent(
    tx: &rusqlite::Transaction<'_>,
    mut existing: ScanOperation,
    targets: &[PathBuf],
) -> Result<ScanOperation, StoreError> {
    let incoming_kind = ScanScopeKind::from_targets(targets);
    let merged_kind = existing.scope_kind.merge(incoming_kind);
    // Full 支配 Targeted：整库语义下 target 列表没有意义，清空以免被误读为缩小范围。
    let mut merged = if merged_kind == ScanScopeKind::Full {
        Vec::new()
    } else {
        existing.targets.clone()
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
    let next_dirty = existing.dirty_rev + 1;
    tx.execute(
        "UPDATE scan_operations
         SET targets_json = ?1, scope_kind = ?2, dirty_rev = ?3
         WHERE id = ?4",
        params![
            targets_json(&merged),
            merged_kind.as_str(),
            next_dirty,
            existing.id
        ],
    )?;
    tracing::info!(
        library_id = %existing.library_id,
        op_id = %existing.id,
        added,
        scope_kind = merged_kind.as_str(),
        dirty_rev = next_dirty,
        "扫描请求已并入进行中的扫描操作"
    );
    existing.targets = merged;
    existing.scope_kind = merged_kind;
    existing.dirty_rev = next_dirty;
    Ok(existing)
}

/// 新建一个 queued scan operation（generation 单调递增）。
fn create_scan_operation(
    tx: &rusqlite::Transaction<'_>,
    library_id: &str,
    signature: &str,
    targets: &[PathBuf],
    now_ms: i64,
) -> Result<ScanOperation, StoreError> {
    let incoming_kind = ScanScopeKind::from_targets(targets);
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
