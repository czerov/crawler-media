use domain::{Coverage, DownloaderId, MediaId, Release, Subscribe, SubscribeId, Torrent, UserId};
use rusqlite::{OptionalExtension, params};
use std::str::FromStr;
use subscribe::{QualityFact, SubscribeFacts};

use super::maps::{map_subscribe, torrent_from_json};
use super::{Store, StoreError};

pub struct PendingDownload {
    pub torrent: Torrent,
    pub release_override: Option<Release>,
    /// The Downloader that accepted this Torrent, retained across default changes.
    pub downloader_id: Option<DownloaderId>,
    /// 投递给下载器的时间（unix secs；resource_timing 用）。
    pub submitted_at: Option<i64>,
}

fn release_override_json(release: &Release) -> serde_json::Value {
    serde_json::json!({
        "title": release.title,
        "year": release.year,
        "season": release.season,
        "episode": release.episode,
        "episode_to": release.episode_to,
        "resolution": release.resolution,
        "source": release.source,
        "codec": release.codec,
        "hdr": release.hdr,
    })
}

fn release_from_json(value: Option<&serde_json::Value>) -> Option<Release> {
    let value = value?;
    Some(Release {
        title: value["title"].as_str().unwrap_or_default().to_string(),
        year: value["year"].as_u64().map(|n| n as u16),
        season: value["season"].as_u64().map(|n| n as u32),
        episode: value["episode"].as_u64().map(|n| n as u32),
        episode_to: value["episode_to"].as_u64().map(|n| n as u32),
        resolution: value["resolution"].as_str().map(str::to_string),
        source: value["source"].as_str().map(str::to_string),
        codec: value["codec"].as_str().map(str::to_string),
        hdr: value["hdr"].as_str().map(str::to_string),
        subtitle_language: None,
        audio_language: None,
        group: None,
        confidence: domain::Confidence::High,
        season_inferred: false,
    })
}

fn pending_torrent_json(pending: &PendingDownload) -> String {
    let mut value = serde_json::json!({
        "site_id": pending.torrent.site_id.to_string(),
        "title": pending.torrent.title,
        "enclosure": pending.torrent.enclosure,
        "size_bytes": pending.torrent.size_bytes,
        "seeders": pending.torrent.seeders,
        "free": pending.torrent.free,
        "hr": pending.torrent.hr,
        "imdb_id": pending.torrent.imdb_id,
        "id": pending.torrent.id,
        "leechers": pending.torrent.leechers,
        "snatched": pending.torrent.snatched,
        "upload_time": pending.torrent.upload_time,
        "detail_url": pending.torrent.detail_url,
        "category": pending.torrent.category,
        "downloader_id": pending.downloader_id.map(|id| id.to_string()),
    });
    if let Some(release) = &pending.release_override {
        value["release_override"] = release_override_json(release);
    }
    value.to_string()
}

impl Store {
    pub fn list_subscribes_for_user(&self, user_id: UserId) -> Result<Vec<Subscribe>, StoreError> {
        let mut stmt = self.subscribe.prepare(
            "SELECT id, user_id, media_id, coverage_kind, season, episode_from, episode_to,
                    fetch_mode, filter_id, wash_cut, wash_cut_filter_id, full_season_pack, downloader_id,
                    tracking_state, follow_future, search_interval_secs, keep_old_versions, library_id
             FROM subscribes WHERE user_id = ?1",
        )?;
        let rows = stmt.query_map(params![user_id.to_string()], map_subscribe)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn list_all_subscribes(&self) -> Result<Vec<Subscribe>, StoreError> {
        let mut stmt = self.subscribe.prepare(
            "SELECT id, user_id, media_id, coverage_kind, season, episode_from, episode_to,
                    fetch_mode, filter_id, wash_cut, wash_cut_filter_id, full_season_pack, downloader_id,
                    tracking_state, follow_future, search_interval_secs, keep_old_versions, library_id
             FROM subscribes",
        )?;
        let rows = stmt.query_map([], map_subscribe)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    fn upsert_pending_row(
        &self,
        subscribe_id: SubscribeId,
        score: i32,
        pending: &PendingDownload,
        refresh_submitted_at: bool,
        revive_rejected: bool,
    ) -> Result<(), StoreError> {
        let value_str = pending_torrent_json(pending);
        let submitted_at = pending.submitted_at.unwrap_or_else(super::unix_now);
        // H2: automatic rounds never revive a `rejected` row; only an explicit
        // user submission does.
        let next_state = if revive_rejected {
            "'active'"
        } else {
            "CASE WHEN pending_downloads.state IN ('rejected', 'stalled') THEN pending_downloads.state ELSE 'active' END"
        };
        let submitted = if refresh_submitted_at {
            "submitted_at = excluded.submitted_at,"
        } else {
            ""
        };
        let sql = format!(
            "INSERT INTO pending_downloads (subscribe_id, enclosure, title, score, torrent_json, submitted_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(subscribe_id, enclosure) DO UPDATE SET
               title = excluded.title,
               score = excluded.score,
               torrent_json = excluded.torrent_json,
               {submitted}
               state = {next_state}"
        );
        let sql = sql.as_str();
        self.subscribe.execute(
            sql,
            params![
                subscribe_id.to_string(),
                pending.torrent.enclosure,
                pending.torrent.title,
                score,
                value_str,
                submitted_at
            ],
        )?;
        Ok(())
    }

    /// 增量维护 pending：**绝不删除**已有行，只 upsert 本轮新候选（按
    /// (subscribe_id, enclosure) 主键）。正在下载/做种的任务在下一轮搜索后
    /// 仍保持跟踪；只有 transfer 成功（delete_pending）或显式取消时才删除。
    pub fn merge_pending(
        &self,
        subscribe_id: SubscribeId,
        torrents: &[(i32, PendingDownload)],
    ) -> Result<(), StoreError> {
        for (score, pending) in torrents {
            self.upsert_pending_row(subscribe_id, *score, pending, false, false)?;
        }
        Ok(())
    }

    /// 明确记录一个由下载器提交成功的 pending 任务（更新 submitted_at 为当前时间，重置其 15 分钟清理宽限期）。
    pub fn record_pending_submission(
        &self,
        subscribe_id: SubscribeId,
        score: i32,
        pending: &PendingDownload,
    ) -> Result<(), StoreError> {
        // Single explicit submission (manual delivery): may revive `rejected`.
        self.upsert_pending_row(subscribe_id, score, pending, true, true)
    }

    /// 批量记录由下载器提交成功的 pending 任务（更新 submitted_at 为当前时间，重置其宽限期）。
    pub fn record_pending_submissions(
        &self,
        subscribe_id: SubscribeId,
        torrents: &[(i32, PendingDownload)],
    ) -> Result<(), StoreError> {
        for (score, pending) in torrents {
            self.upsert_pending_row(subscribe_id, *score, pending, true, false)?;
        }
        Ok(())
    }

    /// Explicit user submissions (manual admit): revive `rejected` rows (H2).
    pub fn record_manual_pending_submissions(
        &self,
        subscribe_id: SubscribeId,
        torrents: &[(i32, PendingDownload)],
    ) -> Result<(), StoreError> {
        for (score, pending) in torrents {
            self.upsert_pending_row(subscribe_id, *score, pending, true, true)?;
        }
        Ok(())
    }

    /// Mark wash-cut arrivals vetoed by their probed quality (H2).
    pub fn mark_pending_rejected(
        &self,
        subscribe_id: SubscribeId,
        enclosures: &[String],
    ) -> Result<(), StoreError> {
        for enclosure in enclosures {
            self.subscribe.execute(
                "UPDATE pending_downloads SET state = 'rejected'
                 WHERE subscribe_id = ?1 AND enclosure = ?2",
                params![subscribe_id.to_string(), enclosure],
            )?;
        }
        Ok(())
    }

    /// Enclosures automatic rounds must never pick again (H2).
    pub fn load_rejected_enclosures(
        &self,
        subscribe_id: SubscribeId,
    ) -> Result<std::collections::HashSet<String>, StoreError> {
        let mut stmt = self.subscribe.prepare(
            "SELECT enclosure FROM pending_downloads WHERE subscribe_id = ?1 AND state IN ('rejected', 'stalled')",
        )?;
        let rows = stmt.query_map(params![subscribe_id.to_string()], |row| {
            row.get::<_, String>(0)
        })?;
        let mut out = std::collections::HashSet::new();
        for row in rows {
            out.insert(row?);
        }
        Ok(out)
    }

    /// D-L4: active pending rows routed to a deleted downloader become
    /// `orphan`: Transfer stops polling them, and (unlike `rejected`) the
    /// release may be picked again for another downloader.
    pub fn mark_pending_orphaned(&self, downloader_id: DownloaderId) -> Result<usize, StoreError> {
        Ok(self.subscribe.execute(
            "UPDATE pending_downloads SET state = 'orphan'
             WHERE state = 'active' AND json_extract(torrent_json, '$.downloader_id') = ?1",
            params![downloader_id.to_string()],
        )?)
    }

    pub fn delete_pending(
        &self,
        subscribe_id: SubscribeId,
        enclosure: &str,
    ) -> Result<(), StoreError> {
        self.subscribe.execute(
            "DELETE FROM pending_downloads WHERE subscribe_id = ?1 AND enclosure = ?2",
            params![subscribe_id.to_string(), enclosure],
        )?;
        Ok(())
    }

    pub fn load_pending(
        &self,
        subscribe_id: SubscribeId,
    ) -> Result<Vec<(i32, PendingDownload)>, StoreError> {
        self.load_pending_state(subscribe_id, "active")
    }

    /// 按 state 取 pending：'active' 在途 / 'imported' 已入库（保留源链接，
    /// 库文件缺失时 transfer 据此重新硬链接）。
    pub fn load_pending_state(
        &self,
        subscribe_id: SubscribeId,
        state: &str,
    ) -> Result<Vec<(i32, PendingDownload)>, StoreError> {
        let mut stmt = self.subscribe.prepare(
            "SELECT score, torrent_json, submitted_at FROM pending_downloads
             WHERE subscribe_id = ?1 AND state = ?2",
        )?;
        let rows = stmt.query_map(params![subscribe_id.to_string(), state], |row| {
            Ok((
                row.get::<_, i32>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (score, raw, submitted_at) = row?;
            let value: serde_json::Value = serde_json::from_str(&raw)?;
            out.push((
                score,
                PendingDownload {
                    torrent: torrent_from_json(&value)?,
                    release_override: release_from_json(value.get("release_override")),
                    downloader_id: value
                        .get("downloader_id")
                        .and_then(|id| id.as_str())
                        .map(DownloaderId::from_str)
                        .transpose()?,
                    submitted_at: (submitted_at > 0).then_some(submitted_at),
                },
            ));
        }
        Ok(out)
    }

    /// 删除条目/单集时清理该 Media 的 imported 源链接：删除就是删除，
    /// 不留任何可被重新收集的尾巴。
    pub fn delete_imported_pending_for_media(
        &self,
        media_id: MediaId,
    ) -> Result<usize, StoreError> {
        let n = self.subscribe.execute(
            "DELETE FROM pending_downloads WHERE state = 'imported' AND subscribe_id IN
               (SELECT id FROM subscribes WHERE media_id = ?1)",
            params![media_id.to_string()],
        )?;
        Ok(n)
    }

    /// 转存成功后把 pending 标记为 imported（不删除）——保留「哪个种子 → 哪个库文件」
    /// 的源链接（当前仅作留档，不自动重新收集）。
    pub fn mark_pending_imported(
        &self,
        subscribe_id: SubscribeId,
        enclosures: &[String],
    ) -> Result<(), StoreError> {
        for enclosure in enclosures {
            self.subscribe.execute(
                "UPDATE pending_downloads SET state = 'imported'
                 WHERE subscribe_id = ?1 AND enclosure = ?2",
                params![subscribe_id.to_string(), enclosure],
            )?;
        }
        Ok(())
    }

    pub fn list_all_pending_routed(
        &self,
    ) -> Result<Vec<(SubscribeId, i32, PendingDownload)>, StoreError> {
        let mut stmt = self.subscribe.prepare(
            "SELECT subscribe_id, score, torrent_json, submitted_at FROM pending_downloads WHERE state = 'active'",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i32>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (subscribe_id, score, raw, submitted_at) = row?;
            let value: serde_json::Value = serde_json::from_str(&raw)?;
            out.push((
                SubscribeId::from_str(&subscribe_id)?,
                score,
                PendingDownload {
                    torrent: torrent_from_json(&value)?,
                    release_override: release_from_json(value.get("release_override")),
                    downloader_id: value
                        .get("downloader_id")
                        .and_then(|id| id.as_str())
                        .map(DownloaderId::from_str)
                        .transpose()?,
                    submitted_at: (submitted_at > 0).then_some(submitted_at),
                },
            ));
        }
        Ok(out)
    }

    pub fn list_all_pending(&self) -> Result<Vec<(SubscribeId, i32, Torrent)>, StoreError> {
        Ok(self
            .list_all_pending_routed()?
            .into_iter()
            .map(|(id, score, pending)| (id, score, pending.torrent))
            .collect())
    }

    pub fn delete_subscribe(&self, id: SubscribeId) -> Result<bool, StoreError> {
        let tx = self.subscribe.unchecked_transaction()?;
        for table in ["pending_downloads", "subscribe_facts", "subscribe_wanted"] {
            tx.execute(
                &format!("DELETE FROM {table} WHERE subscribe_id = ?1"),
                params![id.to_string()],
            )?;
        }
        let n = tx.execute(
            "DELETE FROM subscribes WHERE id = ?1",
            params![id.to_string()],
        )?;
        tx.commit()?;
        // L4: per-subscription settings (`subscribe.<name>:<id>`, e.g. the
        // completion notice marker) live in the app database; drop them too.
        self.app.execute(
            "DELETE FROM settings WHERE key LIKE 'subscribe.%:' || ?1",
            params![id.to_string()],
        )?;
        Ok(n > 0)
    }

    pub fn set_playback_progress(
        &self,
        user_id: UserId,
        media_id: MediaId,
        position_ms: i64,
    ) -> Result<(), StoreError> {
        self.upsert_unit(
            user_id,
            media_id,
            crate::UNIT_WHOLE,
            crate::UNIT_WHOLE,
            position_ms,
            None,
            None,
            None,
            None,
            None,
            false,
            super::unix_now(),
        )?;
        Ok(())
    }

    /// Upsert playback state: position + optional played/favorite/season/episode.
    /// `played`/`favorite` as None keep the stored value (partial update).
    #[allow(clippy::too_many_arguments)]
    pub fn set_playback_state(
        &self,
        user_id: UserId,
        media_id: MediaId,
        position_ms: i64,
        played: Option<bool>,
        favorite: Option<bool>,
        season: Option<u32>,
        episode: Option<u32>,
    ) -> Result<(), StoreError> {
        let (season, episode) = match (season, episode) {
            (Some(s), Some(e)) => (s as i32, e as i32),
            _ => (crate::UNIT_WHOLE, crate::UNIT_WHOLE),
        };
        self.upsert_unit(
            user_id,
            media_id,
            season,
            episode,
            position_ms,
            played,
            favorite,
            None,
            None,
            None,
            false,
            super::unix_now(),
        )?;
        Ok(())
    }

    /// Media-level marks: any played / any favorite across the user's units.
    pub fn playback_marks(
        &self,
        user_id: UserId,
        media_id: MediaId,
    ) -> Result<Option<(bool, bool)>, StoreError> {
        let rows = self.unit_rows(user_id, media_id)?;
        if rows.is_empty() {
            return Ok(None);
        }
        Ok(Some((
            rows.iter().any(|row| row.played),
            rows.iter().any(|row| row.favorite),
        )))
    }

    /// Resume position for a specific unit (season, episode) of a media.
    pub fn playback_unit_progress(
        &self,
        user_id: UserId,
        media_id: MediaId,
        season: Option<u32>,
        episode: Option<u32>,
    ) -> Result<Option<i64>, StoreError> {
        let s: i32 = season.map(|v| v as i32).unwrap_or(-1);
        let e: i32 = episode.map(|v| v as i32).unwrap_or(-1);
        let pos = self
            .subscribe
            .query_row(
                "SELECT position_ms FROM playback_units
                 WHERE user_id = ?1 AND media_id = ?2 AND season = ?3 AND episode = ?4",
                params![user_id.to_string(), media_id.to_string(), s, e],
                |row| row.get::<_, Option<i64>>(0),
            )
            .optional()
            .map_err(StoreError::from)?;
        Ok(pos.flatten())
    }

    /// Resume position for a media: the farthest position across units.
    pub fn playback_progress(
        &self,
        user_id: UserId,
        media_id: MediaId,
    ) -> Result<Option<i64>, StoreError> {
        let max = self
            .subscribe
            .query_row(
                "SELECT MAX(position_ms) FROM playback_units
                 WHERE user_id = ?1 AND media_id = ?2",
                params![user_id.to_string(), media_id.to_string()],
                |row| row.get::<_, Option<i64>>(0),
            )
            .optional()
            .map_err(StoreError::from)?;
        Ok(max.flatten())
    }

    /// All playback progress rows `(user_id, media_id, position_ms)`.
    pub fn list_playback_progress(&self) -> Result<Vec<(UserId, MediaId, i64)>, StoreError> {
        let mut stmt = self.subscribe.prepare(
            "SELECT user_id, media_id, MAX(position_ms) FROM playback_units
             GROUP BY user_id, media_id ORDER BY MAX(position_ms) DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            let user = UserId::from_str(&row.get::<_, String>(0)?).map_err(|err| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(err),
                )
            })?;
            let media = MediaId::from_str(&row.get::<_, String>(1)?).map_err(|err| {
                rusqlite::Error::FromSqlConversionFailure(
                    1,
                    rusqlite::types::Type::Text,
                    Box::new(err),
                )
            })?;
            Ok((user, media, row.get::<_, i64>(2)?))
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }
    pub fn insert_subscribe(&self, subscribe: &Subscribe) -> Result<(), StoreError> {
        let (coverage_kind, season, episode_from, episode_to) = match &subscribe.coverage {
            Coverage::Movie => ("movie", None, None, None),
            Coverage::Tv {
                season,
                episode_from,
                episode_to,
            } => (
                "tv",
                Some(*season as i64),
                Some(*episode_from as i64),
                episode_to.map(|v| v as i64),
            ),
        };
        self.subscribe.execute(
            "INSERT INTO subscribes (
                id, user_id, media_id, coverage_kind, season, episode_from, episode_to,
                fetch_mode, filter_id, wash_cut, wash_cut_filter_id, full_season_pack, downloader_id,
                tracking_state, follow_future, search_interval_secs, keep_old_versions, library_id, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)",
            params![
                subscribe.id.to_string(),
                subscribe.user_id.to_string(),
                subscribe.media_id.to_string(),
                coverage_kind,
                season,
                episode_from,
                episode_to,
                subscribe.fetch_mode.as_str(),
                subscribe.filter_id.to_string(),
                subscribe.wash_cut as i64,
                subscribe.wash_cut_filter_id.map(|id| id.to_string()),
                subscribe.full_season_pack as i64,
                subscribe.downloader_id.map(|id| id.to_string()),
                subscribe.tracking_state,
                subscribe.follow_future as i64,
                subscribe.search_interval_secs as i64,
                subscribe.keep_old_versions as i64,
                subscribe.library_id.map(|id| id.to_string()),
                super::now_rfc3339(),
                super::now_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn get_subscribe(&self, id: SubscribeId) -> Result<Option<Subscribe>, StoreError> {
        self.subscribe
            .query_row(
                "SELECT id, user_id, media_id, coverage_kind, season, episode_from, episode_to,
                        fetch_mode, filter_id, wash_cut, wash_cut_filter_id, full_season_pack, downloader_id,
                        tracking_state, follow_future, search_interval_secs, keep_old_versions, library_id
                 FROM subscribes WHERE id = ?1",
                params![id.to_string()],
                map_subscribe,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn update_subscribe(&self, subscribe: &Subscribe) -> Result<(), StoreError> {
        let (coverage_kind, season, episode_from, episode_to) = match &subscribe.coverage {
            Coverage::Movie => ("movie", None, None, None),
            Coverage::Tv {
                season,
                episode_from,
                episode_to,
            } => (
                "tv",
                Some(*season as i64),
                Some(*episode_from as i64),
                episode_to.map(|v| v as i64),
            ),
        };
        self.subscribe.execute(
            "UPDATE subscribes SET
                fetch_mode = ?1, filter_id = ?2, wash_cut = ?3, wash_cut_filter_id = ?4,
                full_season_pack = ?5, downloader_id = ?6,
                coverage_kind = ?7, season = ?8, episode_from = ?9, episode_to = ?10,
                tracking_state = ?12, follow_future = ?13, search_interval_secs = ?14,
                keep_old_versions = ?16, library_id = ?17, updated_at = ?15
             WHERE id = ?11",
            params![
                subscribe.fetch_mode.as_str(),
                subscribe.filter_id.to_string(),
                subscribe.wash_cut as i64,
                subscribe.wash_cut_filter_id.map(|id| id.to_string()),
                subscribe.full_season_pack as i64,
                subscribe.downloader_id.map(|id| id.to_string()),
                coverage_kind,
                season,
                episode_from,
                episode_to,
                subscribe.id.to_string(),
                subscribe.tracking_state,
                subscribe.follow_future as i64,
                subscribe.search_interval_secs as i64,
                super::now_rfc3339(),
                subscribe.keep_old_versions as i64,
                subscribe.library_id.map(|id| id.to_string()),
            ],
        )?;
        Ok(())
    }

    /// `(created_at, updated_at)` RFC3339 for a Subscribe; `("", "")` when the
    /// row predates the timestamp migration and was never touched.
    pub fn subscribe_times(&self, id: SubscribeId) -> Result<(String, String), StoreError> {
        let row = self
            .subscribe
            .query_row(
                "SELECT created_at, updated_at FROM subscribes WHERE id = ?1",
                params![id.to_string()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        Ok(row.unwrap_or_default())
    }

    /// 把内存里的订阅事实写回 DB。内存层已是唯一判定点：`upsert` 只接受
    /// 更高分（防陈旧 pending 回退），`replace` 写入 chooser 批准的替换
    /// （含 UpgradeLadder 同分/低分更高维度）。这里必须**无条件镜像**内存值：
    /// 早期版本在 SQL 里再做一次 `excluded.score > score` 守门，导致洗版
    /// 批准的同分替换（新文件已入库、旧文件已回收）写不进去，DB 永远指向
    /// 已被删除的旧路径，下一轮重复批准洗版。
    pub fn save_subscribe_facts(
        &self,
        subscribe_id: SubscribeId,
        facts: &SubscribeFacts,
    ) -> Result<(), StoreError> {
        self.persist_owned_quality(facts)?;
        let tx = self.subscribe.unchecked_transaction()?;
        let mut statement = tx.prepare(
            "INSERT INTO subscribe_facts (subscribe_id, season, episode, score, path)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(subscribe_id, season, episode) DO UPDATE SET
               score = excluded.score,
               path = excluded.path",
        )?;
        for ((season, episode), fact) in facts.entries().filter(|(_, fact)| fact.path.is_some()) {
            statement.execute(params![
                subscribe_id.to_string(),
                season.map_or(-1i64, |s| s as i64),
                episode.map_or(-1i64, |e| e as i64),
                fact.score,
                fact.path,
            ])?;
        }
        drop(statement);
        tx.execute(
            "DELETE FROM subscribe_facts WHERE subscribe_id = ?1 AND path IS NULL",
            [subscribe_id.to_string()],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn load_subscribe_facts(
        &self,
        subscribe_id: SubscribeId,
    ) -> Result<SubscribeFacts, StoreError> {
        let mut statement = self.subscribe.prepare(
            "SELECT season, episode, score, path
             FROM subscribe_facts WHERE subscribe_id = ?1 AND path IS NOT NULL",
        )?;
        let rows = statement.query_map(params![subscribe_id.to_string()], |row| {
            let season: i64 = row.get(0)?;
            let episode: i64 = row.get(1)?;
            Ok((
                (season >= 0).then_some(season as u32),
                (episode >= 0).then_some(episode as u32),
                QualityFact {
                    score: row.get(2)?,
                    path: row.get(3)?,
                },
            ))
        })?;
        let mut facts = SubscribeFacts::default();
        for row in rows {
            let (season, episode, fact) = row?;
            if let Some(path) = fact.path.as_deref() {
                if let Some(ledger) = self.ledger_by_path(path)? {
                    facts.set_quality(path.to_string(), self.owned_quality(&ledger)?);
                }
            }
            facts.upsert(season, episode, fact);
        }
        Ok(facts)
    }
}
