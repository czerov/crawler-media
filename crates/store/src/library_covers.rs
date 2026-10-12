//! Library 封面指针的版本化发布（C2b）。
//!
//! 渲染结果先写成新的不可变版本文件，DB 指针最后通过 CAS 发布：只有当前
//! `cover_revision` 仍等于渲染时读到的值，才写入新的 `cover_path` 并把
//! revision +1。晚到的渲染因此无法覆盖更新的手动封面或 `none` 标记。

use rusqlite::{OptionalExtension, params};

use crate::{Store, StoreError};

/// 封面检查标记在 `settings` 里的 key（key 语义由 api 层定义，store 只负责持久化）。
fn marker_key(library_id: &str) -> String {
    format!("library_cover_checked.{library_id}")
}

impl Store {
    /// CAS：仅当当前 revision == expected 时写入 path/marker 并 +1。返回是否命中。
    ///
    /// 单连接事务内完成 checked UPDATE；0 行受影响时返回 `false` 并回滚，
    /// 调用方必须删除那个尚未被引用的新版本文件。
    pub fn compare_set_library_cover(
        &self,
        id: &str,
        expected_revision: i64,
        path: Option<&str>,
        marker: &str,
    ) -> Result<bool, StoreError> {
        self.ensure_library_mutable(id)?;
        let tx = self.app.unchecked_transaction()?;
        let changed = tx.execute(
            "UPDATE libraries SET cover_path = ?1, cover_revision = cover_revision + 1
             WHERE id = ?2 AND cover_revision = ?3",
            params![path, id, expected_revision],
        )?;
        if changed == 0 {
            return Ok(false);
        }
        tx.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
            params![marker_key(id), marker],
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// 当前封面指针 revision；Library 不存在时返回 `None`。
    pub fn library_cover_revision(&self, id: &str) -> Result<Option<i64>, StoreError> {
        self.app
            .query_row(
                "SELECT COALESCE(cover_revision, 0) FROM libraries WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }
}
