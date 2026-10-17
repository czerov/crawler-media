//! A4 浏览页事实：`browse_media` 的 SQL 排序键列与索引（幂等迁移）。
//!
//! 排序键列是 `candidate_json` 的同源物化副本（与既有 `title`/`year` 列同一套
//! 做法），只服务「常用排序用索引直接分页」：`added_at_ns` 是最早入库时间（与
//! `file_stats` 同口径，缺失为 NULL，ASC 时 NULL 在前，与 Rust 的 `Option` 序一致），
//! `release_date` 是物化后的排序键（缺失为空串）。旧库补列后仍缺候选 JSON，
//! 读路径据此判定投影过旧并回退全量读取。

use rusqlite::Connection;

use super::ensure_column;
use crate::StoreError;

pub fn migrate_browse_page_facts(conn: &Connection) -> Result<(), StoreError> {
    ensure_column(conn, "browse_media", "added_at_ns", "INTEGER")?;
    ensure_column(conn, "browse_media", "release_date", "TEXT NOT NULL DEFAULT ''")?;
    ensure_column(conn, "browse_media", "candidate_json", "TEXT NOT NULL DEFAULT ''")?;
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS browse_media_added_at_idx
             ON browse_media(library_id, added_at_ns, media_id);
         CREATE INDEX IF NOT EXISTS browse_media_release_date_idx
             ON browse_media(library_id, release_date, media_id);",
    )?;
    Ok(())
}

/// 升级后标脏旧投影：旧代的 `browse_media` 行没有候选 JSON，facts 版本检查只会让
/// 单次请求回退到全量读取、不会自己重建，因此必须在迁移时显式要求重建一次。
/// 只影响已被浏览过的库（`browse_generations` 只有这些行）。
pub fn invalidate_legacy_projections(conn: &Connection) -> Result<(), StoreError> {
    conn.execute("UPDATE browse_generations SET state = 'dirty'", [])?;
    Ok(())
}
