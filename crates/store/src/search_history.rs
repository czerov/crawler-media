//! 搜索历史的**单事务**删除。
//!
//! `settings`（历史条目）与 `search_snapshots`（结果快照）都在 app.db，两步写入
//! 必须落在同一个事务里：先删快照再改设置，一旦设置写失败就留下「条目还在、
//! 快照没了」的死链——点开历史返回 404。这里用 `app_write` 把两步绑在一起，
//! 任一步失败整体回滚。

use super::{Store, StoreError};

/// 一次历史删除在事务内要执行的写入。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryDeletePlan {
    /// 要删除的快照 id（单条删除时最多一个，清空时是该用户全部历史 id）。
    pub snapshot_ids: Vec<String>,
    /// 重写后的历史设置值；`None` 表示删除该设置键（清空历史）。
    pub settings_value: Option<String>,
}

/// 事务执行结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryDeleteOutcome {
    /// 历史条目是否真的存在。`false` 表示 `plan` 判定无事可做，事务没有写入。
    pub deleted: bool,
    /// 实际删掉的快照行数（用于「条目在、快照本就没落盘」的告警）。
    pub snapshots_removed: usize,
}

impl Store {
    /// 在同一 app.db 事务里删除快照行并重写历史设置。
    ///
    /// `plan` 在事务内读到当前设置原文（`None` = 键不存在），返回 `None` 表示这条
    /// 历史本来就不在：不删快照、不改设置。返回 `Some` 时先删快照、再写设置，
    /// 任一步返回 `StoreError` 都由 `app_write` 回滚，因此调用方看到 `Err` 时
    /// 条目与快照一定都还是原样。
    pub fn delete_search_history(
        &self,
        settings_key: &str,
        plan: impl FnOnce(Option<&str>) -> Option<HistoryDeletePlan>,
    ) -> Result<HistoryDeleteOutcome, StoreError> {
        self.app_write(|store| {
            let current = store.get_setting(settings_key)?;
            let Some(plan) = plan(current.as_deref()) else {
                return Ok(HistoryDeleteOutcome {
                    deleted: false,
                    snapshots_removed: 0,
                });
            };
            let mut snapshots_removed = 0usize;
            for id in &plan.snapshot_ids {
                if store.delete_search_snapshot(id)? {
                    snapshots_removed += 1;
                }
            }
            match plan.settings_value {
                Some(value) => store.put_setting(settings_key, &value)?,
                None => {
                    store.delete_setting(settings_key)?;
                }
            }
            Ok(HistoryDeleteOutcome {
                deleted: true,
                snapshots_removed,
            })
        })
    }
}
