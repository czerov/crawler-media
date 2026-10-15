//! Scan version checkpoints and durable source versions on ledger.

use std::collections::HashMap;
use std::path::Path;

use rusqlite::params;

use super::{Store, StoreError};

impl Store {
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
    ///
    /// 同一事务里还会把版本写进台账行的 durable 源版本（P2-3）：`scan_seen` 只是当前
    /// operation 的断点，终态即清空；跨 operation 的变化检测靠 ledger 上的这份事实。
    /// 因此「断点」与「durable 版本」永远一起前进，不会出现断点已写而版本没写。
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
            // 只有已登记的台账行才有 durable 源版本；没有台账行的路径这里是空操作。
            tx.execute(
                "UPDATE ledger
                    SET source_version_ms = ?2, source_version_size = ?3,
                        source_version_digest = ?4
                  WHERE path = ?1",
                params![path, mtime_ms, file_size, content_digest],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// 读 durable 每台账源版本：`path -> (mtime_ms, file_size, content_digest)`（P2-3）。
    ///
    /// 返回的是「上次处理该路径时的文件版本」，跨 operation 有效。台账行存在但从未
    /// 记录过版本时返回 `0/0/NULL`——按既有约定等价「版本未知」，由
    /// [`Store::scan_seen_stamp_matches`] 判为不可信，必须重新处理。map 里没有的路径
    /// 表示根本没有台账行（本轮新文件）。
    pub fn ledger_source_versions(
        &self,
        paths: &[String],
    ) -> Result<HashMap<String, (i64, i64, Option<String>)>, StoreError> {
        let mut versions = HashMap::new();
        if paths.is_empty() {
            return Ok(versions);
        }
        // 分块：复用 ledger.path 索引，同时不触碰 SQLite 的变量上限。
        for chunk in paths.chunks(500) {
            let placeholders: Vec<String> = (1..=chunk.len()).map(|i| format!("?{i}")).collect();
            let sql = format!(
                "SELECT path, source_version_ms, source_version_size, source_version_digest
                 FROM ledger WHERE path IN ({})",
                placeholders.join(",")
            );
            let mut stmt = self.library.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    (row.get(1)?, row.get(2)?, row.get(3)?),
                ))
            })?;
            for row in rows {
                let (path, version) = row?;
                versions.insert(path, version);
            }
        }
        Ok(versions)
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
    pub fn record_scan_seen(&self, operation_id: &str, paths: &[std::path::PathBuf]) -> Result<(), StoreError> {
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
}

/// 文件版本：mtime 毫秒 + 字节数；读不到时返回 0/0（视为「版本未知」）。
fn file_version(path: &Path) -> (i64, i64) {
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
