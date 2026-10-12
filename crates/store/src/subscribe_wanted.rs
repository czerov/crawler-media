use std::collections::HashMap;

use domain::SubscribeId;
use rusqlite::params;

use super::{Store, StoreError};

/// 工单（wanted）单元的搜索/投递历史。状态本身按 facts（imported）/
/// pending（grabbed）实时推导，这里只存会随时间变化的履历字段。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WantedHistory {
    pub search_attempts: i32,
    pub last_search_at: Option<i64>,
    pub grabbed_at: Option<i64>,
    pub imported_at: Option<i64>,
    pub grab_title: Option<String>,
    pub last_reject_reason: Option<String>,
}

pub(crate) fn wanted_key(season: Option<u32>, episode: Option<u32>) -> (i64, i64) {
    (season.map_or(-1, i64::from), episode.map_or(-1, i64::from))
}

impl Store {
    /// 批量 wanted 更新的事务 seam：在 subscribe 单连接上包一个事务，
    /// 让「24 集」只有一次提交、失败时整体回滚。调用方已经在事务里时
    /// （`!is_autocommit()`）直接执行——与 `playback_write` 同一约定，
    /// 不嵌套开事务。
    fn wanted_write<T>(
        &self,
        operation: impl FnOnce(&Self) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        if !self.subscribe.is_autocommit() {
            return operation(self);
        }
        let transaction = self.subscribe.unchecked_transaction()?;
        let result = operation(self)?;
        transaction.commit()?;
        Ok(result)
    }

    /// 一轮搜索后：对每个还在追踪的单元累加 search_attempts 并刷新 last_search_at。
    /// 已入库（imported）的单元不在轮次里，不动。
    pub fn touch_wanted_searches(
        &self,
        subscribe_id: SubscribeId,
        units: &[(Option<u32>, Option<u32>)],
        now: i64,
    ) -> Result<(), StoreError> {
        self.wanted_write(|store| {
            let mut statement = store.subscribe.prepare(
                "INSERT INTO subscribe_wanted (subscribe_id, season, episode, search_attempts, last_search_at)
                 VALUES (?1, ?2, ?3, 1, ?4)
                 ON CONFLICT(subscribe_id, season, episode) DO UPDATE SET
                   search_attempts = search_attempts + 1, last_search_at = excluded.last_search_at",
            )?;
            for (season, episode) in units {
                let (s, e) = wanted_key(*season, *episode);
                statement.execute(params![subscribe_id.to_string(), s, e, now])?;
            }
            Ok(())
        })
    }

    /// Clear retry cooldown fields only for the supplied still-missing units.
    pub fn reset_wanted_search_cooldowns(
        &self,
        subscribe_id: SubscribeId,
        units: &[(Option<u32>, Option<u32>)],
    ) -> Result<usize, StoreError> {
        self.wanted_write(|store| {
            let mut statement = store.subscribe.prepare(
                "UPDATE subscribe_wanted
                 SET search_attempts = 0, last_search_at = NULL
                 WHERE subscribe_id = ?1 AND season = ?2 AND episode = ?3
                   AND (search_attempts <> 0 OR last_search_at IS NOT NULL)",
            )?;
            let mut reset = 0;
            for (season, episode) in units {
                let (s, e) = wanted_key(*season, *episode);
                reset += statement.execute(params![subscribe_id.to_string(), s, e])?;
            }
            Ok(reset)
        })
    }

    /// 候选被规则组拒绝：记录最近一次拒绝原因（站点 · 种子名）。
    pub fn record_wanted_reject(
        &self,
        subscribe_id: SubscribeId,
        season: Option<u32>,
        episode: Option<u32>,
        reason: &str,
        now: i64,
    ) -> Result<(), StoreError> {
        let (s, e) = wanted_key(season, episode);
        self.subscribe.execute(
            "INSERT INTO subscribe_wanted (subscribe_id, season, episode, last_reject_reason, last_search_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(subscribe_id, season, episode) DO UPDATE SET
               last_reject_reason = excluded.last_reject_reason",
            params![subscribe_id.to_string(), s, e, reason, now],
        )?;
        Ok(())
    }

    /// 一个种子被投给下载器：记录抓取时间与种子名。
    pub fn record_wanted_grab(
        &self,
        subscribe_id: SubscribeId,
        season: Option<u32>,
        episode: Option<u32>,
        title: &str,
        now: i64,
    ) -> Result<(), StoreError> {
        let (s, e) = wanted_key(season, episode);
        self.subscribe.execute(
            "INSERT INTO subscribe_wanted (subscribe_id, season, episode, grab_title, grabbed_at, last_search_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)
             ON CONFLICT(subscribe_id, season, episode) DO UPDATE SET
               grab_title = excluded.grab_title,
               grabbed_at = COALESCE(grabbed_at, excluded.grabbed_at)",
            params![subscribe_id.to_string(), s, e, title, now],
        )?;
        Ok(())
    }

    /// 转移完成：该单元已入库。
    pub fn mark_wanted_imported(
        &self,
        subscribe_id: SubscribeId,
        season: Option<u32>,
        episode: Option<u32>,
        now: i64,
    ) -> Result<(), StoreError> {
        let (s, e) = wanted_key(season, episode);
        self.subscribe.execute(
            "INSERT INTO subscribe_wanted (subscribe_id, season, episode, imported_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(subscribe_id, season, episode) DO UPDATE SET
               imported_at = COALESCE(imported_at, excluded.imported_at)",
            params![subscribe_id.to_string(), s, e, now],
        )?;
        Ok(())
    }

    /// 全部工单历史（(season, episode) 键与 facts 同构，-1 表示 movie）。
    pub fn load_wanted_history(
        &self,
        subscribe_id: SubscribeId,
    ) -> Result<HashMap<(Option<u32>, Option<u32>), WantedHistory>, StoreError> {
        let mut statement = self.subscribe.prepare(
            "SELECT season, episode, search_attempts, last_search_at,
                    grabbed_at, imported_at, grab_title, last_reject_reason
             FROM subscribe_wanted WHERE subscribe_id = ?1",
        )?;
        let rows = statement.query_map(params![subscribe_id.to_string()], |row| {
            let season: i64 = row.get(0)?;
            let episode: i64 = row.get(1)?;
            Ok((
                (season >= 0).then_some(season as u32),
                (episode >= 0).then_some(episode as u32),
                WantedHistory {
                    search_attempts: row.get(2)?,
                    last_search_at: row.get(3)?,
                    grabbed_at: row.get(4)?,
                    imported_at: row.get(5)?,
                    grab_title: row.get(6)?,
                    last_reject_reason: row.get(7)?,
                },
            ))
        })?;
        let mut out = HashMap::new();
        for row in rows {
            let (season, episode, history) = row?;
            out.insert((season, episode), history);
        }
        Ok(out)
    }

    /// 退订：连同工单历史一起清理。
    pub fn delete_wanted(&self, subscribe_id: SubscribeId) -> Result<(), StoreError> {
        self.subscribe.execute(
            "DELETE FROM subscribe_wanted WHERE subscribe_id = ?1",
            params![subscribe_id.to_string()],
        )?;
        Ok(())
    }
}
