use std::path::Path;

use domain::LedgerRow;
use rusqlite::{OptionalExtension, params};

use super::maps::map_ledger;
use super::{Store, StoreError};

impl Store {
    pub fn ledger_source_paths(
        &self,
        media_id: domain::MediaId,
    ) -> Result<Vec<String>, StoreError> {
        let mut stmt = self.library.prepare(
            "SELECT source_path FROM ledger WHERE media_id = ?1 AND source_path IS NOT NULL",
        )?;
        let rows = stmt.query_map([media_id.to_string()], |row| row.get(0))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn insert_ledger(&self, row: &LedgerRow) -> Result<(), StoreError> {
        self.insert_ledger_with_optional_source(row, None)
    }

    /// Save the original completed download path alongside an imported row.
    /// The source is retained for an explicit administrator-triggered
    /// retransfer when the Library copy later goes missing.
    pub fn insert_ledger_with_source(
        &self,
        row: &LedgerRow,
        source: &Path,
    ) -> Result<(), StoreError> {
        self.insert_ledger_with_optional_source(row, source.to_str())
    }

    /// Insert ledger row with caller-observed transfer mode and optional source path,
    /// avoiding filesystem metadata inspection under the Store lock.
    pub fn insert_ledger_with_mode_and_source(
        &self,
        row: &LedgerRow,
        transfer_mode: Option<&str>,
        source_path: Option<&str>,
    ) -> Result<(), StoreError> {
        let mode = match transfer_mode {
            Some(m) => m.to_string(),
            None => self.transfer_mode_for_path(&row.path),
        };
        self.library.execute(
            "INSERT INTO ledger (
                id, media_id, path, season, episode, resolution, codec, hdr,
                quality_source, confidence, filter_score, transfer_mode, source_path
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
             ON CONFLICT(path) DO UPDATE SET
                media_id = excluded.media_id,
                season = excluded.season,
                episode = excluded.episode,
                resolution = excluded.resolution,
                codec = excluded.codec,
                hdr = excluded.hdr,
                quality_source = excluded.quality_source,
                confidence = excluded.confidence,
                filter_score = excluded.filter_score,
                transfer_mode = COALESCE(excluded.transfer_mode, ledger.transfer_mode),
                source_path = COALESCE(excluded.source_path, ledger.source_path)",
            params![
                row.id.to_string(),
                row.media_id.to_string(),
                row.path,
                row.season.map(i64::from),
                row.episode.map(i64::from),
                row.resolution,
                row.codec,
                row.hdr,
                row.quality_source.as_str(),
                row.confidence.as_str(),
                row.filter_score,
                mode,
                source_path,
            ],
        )?;
        Ok(())
    }

    /// Finalize a previously reserved Transfer row without recreating it if detached.
    pub fn finalize_transfer(
        &self,
        row: &LedgerRow,
        transfer_mode: &str,
        source_path: Option<&str>,
    ) -> Result<(), StoreError> {
        let changed = self.library.execute(
            "UPDATE ledger SET season = ?4, episode = ?5, resolution = ?6, codec = ?7,
                hdr = ?8, quality_source = ?9, confidence = ?10, filter_score = ?11,
                transfer_mode = COALESCE(?12, transfer_mode)
             WHERE id = ?1 AND media_id = ?2 AND path = ?3 AND source_path IS ?13",
            params![
                row.id.to_string(), row.media_id.to_string(), row.path,
                row.season.map(i64::from), row.episode.map(i64::from), row.resolution,
                row.codec, row.hdr, row.quality_source.as_str(), row.confidence.as_str(),
                row.filter_score, transfer_mode, source_path,
            ],
        )?;
        if changed != 1 {
            return Err(StoreError::Protected("Transfer reservation changed or was detached".into()));
        }
        Ok(())
    }

    pub fn ledger_source_path_matches(
        &self,
        path: &str,
        expected: Option<&str>,
    ) -> Result<bool, StoreError> {
        let actual: Option<Option<String>> = self.library.query_row(
            "SELECT source_path FROM ledger WHERE path = ?1",
            [path],
            |row| row.get(0),
        ).optional()?;
        Ok(actual.flatten().as_deref() == expected)
    }

    /// Reserve a final path before filesystem publication. Existing identity is
    /// never overwritten; only the same in-flight source can resume publication.
    pub fn prepare_transfer(&self, row: &LedgerRow, mode: &str, source: &str)
        -> Result<LedgerRow, StoreError>
    {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.library, rusqlite::TransactionBehavior::Immediate,
        )?;
        if let Some(existing) = self.ledger_by_path(&row.path)? {
            let owned: bool = tx.query_row(
                "SELECT transfer_pending = 1 AND COALESCE(source_path = ?2, 0) FROM ledger WHERE path = ?1",
                params![row.path, source], |r| r.get(0),
            )?;
            if existing.media_id != row.media_id || !owned {
                return Err(StoreError::Protected("Transfer destination already owned".into()));
            }
            tx.commit()?;
            return Ok(existing);
        }
        self.insert_ledger_with_mode_and_source(row, Some(mode), Some(source))?;
        tx.execute("UPDATE ledger SET transfer_pending = 1 WHERE id = ?1", [row.id.to_string()])?;
        tx.commit()?;
        tracing::info!(ledger_id = %row.id, path = %row.path, "Reserved Transfer destination");
        Ok(row.clone())
    }

    pub fn transfer_pending(&self, path: &str) -> Result<bool, StoreError> {
        Ok(self.library.query_row("SELECT transfer_pending FROM ledger WHERE path = ?1",
            [path], |r| r.get(0)).optional()?.unwrap_or(false))
    }

    pub fn complete_transfer_publication(&self, row: &LedgerRow) -> Result<(), StoreError> {
        let changed = self.library.execute(
            "UPDATE ledger SET transfer_pending = 0 WHERE id = ?1 AND media_id = ?2 AND path = ?3",
            params![row.id.to_string(), row.media_id.to_string(), row.path],
        )?;
        if changed != 1 {
            return Err(StoreError::Protected("Transfer reservation changed".into()));
        }
        tracing::info!(ledger_id = %row.id, path = %row.path, "Completed Transfer publication");
        Ok(())
    }

    fn insert_ledger_with_optional_source(
        &self,
        row: &LedgerRow,
        source_path: Option<&str>,
    ) -> Result<(), StoreError> {
        self.insert_ledger_with_mode_and_source(row, None, source_path)
    }

    fn transfer_mode_for_path(&self, path: &str) -> String {
        let p = std::path::Path::new(path);
        if p.extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("strm"))
        {
            return "strm".to_string();
        }

        let is_hardlink = std::fs::metadata(path)
            .map(|meta| {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    meta.nlink() > 1
                }
                #[cfg(not(unix))]
                {
                    false
                }
            })
            .unwrap_or(false);
        if is_hardlink {
            "hardlink".to_string()
        } else {
            self.transfer_mode().unwrap_or_else(|_| "copy".into())
        }
    }

    pub fn delete_ledger_path(&self, path: &str) -> Result<(), StoreError> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.library,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let ledger_id: Option<String> = tx
            .query_row("SELECT id FROM ledger WHERE path = ?1", [path], |row| {
                row.get(0)
            })
            .optional()?;
        if let Some(id) = ledger_id {
            super::ledger_cleanup::delete_ledger_by_id_str_in_tx(&tx, &id)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// 按路径取已有台账行（扫描入库时判断「已记录」用）。
    pub fn ledger_by_path(&self, path: &str) -> Result<Option<LedgerRow>, StoreError> {
        self.library
            .query_row(
                "SELECT id, media_id, path, season, episode, resolution, codec, hdr,
                        quality_source, confidence, filter_score
                 FROM ledger WHERE path = ?1",
                params![path],
                map_ledger,
            )
            .optional()
            .map_err(Into::into)
    }

    /// Rename a ledger row's path (整理后落名一致；path 唯一约束保持）。
    pub fn rename_ledger_path(&self, from: &str, to: &str) -> Result<bool, StoreError> {
        let n = self.library.execute(
            "UPDATE ledger SET path = ?2 WHERE path = ?1",
            params![from, to],
        )?;
        Ok(n > 0)
    }

    /// Update media stream facts (resolution, codec, hdr) in ledger row once probed.
    pub fn update_ledger_probe_quality(
        &self,
        ledger_id: &str,
        resolution: Option<&str>,
        codec: Option<&str>,
        hdr: Option<&str>,
    ) -> Result<bool, StoreError> {
        let n = self.library.execute(
            "UPDATE ledger
             SET resolution = COALESCE(?2, resolution),
                 codec = COALESCE(?3, codec),
                 hdr = COALESCE(?4, hdr)
             WHERE id = ?1",
            params![ledger_id, resolution, codec, hdr],
        )?;
        Ok(n > 0)
    }

    /// Rewrite every fact path equal to `from` into `to` for a media's
    /// subscribes（整理后 wash-cut 的事实路径仍指向新文件）。
    pub fn rewrite_fact_paths(
        &self,
        media_id: domain::MediaId,
        from: &str,
        to: &str,
    ) -> Result<(), StoreError> {
        for subscribe in self.list_all_subscribes()? {
            if subscribe.media_id != media_id {
                continue;
            }
            let mut facts = self.load_subscribe_facts(subscribe.id)?;
            let mut changed = false;
            for entry in facts.entries_mut() {
                let fact = entry.1;
                if fact.path.as_deref() == Some(from) {
                    fact.path = Some(to.to_string());
                    changed = true;
                }
            }
            if changed {
                self.save_subscribe_facts(subscribe.id, &facts)?;
            }
        }
        Ok(())
    }

    pub fn ledger_for_media(
        &self,
        media_id: domain::MediaId,
    ) -> Result<Vec<LedgerRow>, StoreError> {
        let mut stmt = self.library.prepare(
            "SELECT id, media_id, path, season, episode, resolution, codec, hdr,
                    quality_source, confidence, filter_score
             FROM ledger WHERE media_id = ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![media_id.to_string()], map_ledger)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// 一次取多个 Media 的 ledger 行（≤500 个 id 一块，复用 `ledger.media_id` 索引）。
    /// 重复 id 只查询一次，输出行不保证顺序。
    pub fn ledger_for_media_batch(
        &self,
        ids: &[domain::MediaId],
    ) -> Result<Vec<LedgerRow>, StoreError> {
        let unique: std::collections::HashSet<domain::MediaId> = ids.iter().copied().collect();
        let mut out = Vec::new();
        let ids: Vec<domain::MediaId> = unique.into_iter().collect();
        for chunk in ids.chunks(500) {
            let placeholders = vec!["?"; chunk.len()].join(",");
            let sql = format!(
                "SELECT id, media_id, path, season, episode, resolution, codec, hdr,
                        quality_source, confidence, filter_score
                 FROM ledger WHERE media_id IN ({placeholders})"
            );
            let mut stmt = self.library.prepare(&sql)?;
            let rows = stmt.query_map(
                rusqlite::params_from_iter(chunk.iter().map(domain::MediaId::to_string)),
                map_ledger,
            )?;
            out.extend(rows.collect::<Result<Vec<_>, _>>()?);
        }
        Ok(out)
    }
    pub fn get_ledger_by_unit(
        &self,
        media_id: domain::MediaId,
        season: Option<u32>,
        episode: Option<u32>,
    ) -> Result<Option<LedgerRow>, StoreError> {
        let rows = self.ledger_for_media(media_id)?;
        Ok(rows.into_iter().find(|r| r.season == season && r.episode == episode))
    }

    pub fn delete_ledger_for_media(&self, media_id: domain::MediaId) -> Result<usize, StoreError> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.library,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let ids = tx
            .prepare("SELECT id FROM ledger WHERE media_id = ?1")?
            .query_map([media_id.to_string()], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        let mut changed = 0;
        for id in ids {
            if super::ledger_cleanup::delete_ledger_by_id_str_in_tx(&tx, &id)? {
                changed += 1;
            }
        }
        tx.commit()?;
        Ok(changed)
    }

    pub fn list_ledger(&self) -> Result<Vec<LedgerRow>, StoreError> {
        Ok(self
            .list_ledger_with_mode()?
            .into_iter()
            .map(|(row, _, _)| row)
            .collect())
    }

    pub fn list_ledger_with_mode(
        &self,
    ) -> Result<Vec<(LedgerRow, Option<String>, Option<String>)>, StoreError> {
        let mut stmt = self.library.prepare(
            "SELECT id, media_id, path, season, episode, resolution, codec, hdr,
                    quality_source, confidence, filter_score, transfer_mode, source_path
             FROM ledger",
        )?;
        let rows = stmt.query_map([], |row| {
            let ledger = map_ledger(row)?;
            let mode: Option<String> = row.get(11).ok();
            let source_path: Option<String> = row.get(12).ok();
            Ok((ledger, mode, source_path))
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn get_ledger(&self, compact_id: &str) -> Result<Option<LedgerRow>, StoreError> {
        // compact_id is a UUID without hyphens; try to reconstruct the
        // hyphenated form and query directly instead of a full-table scan.
        if let Ok(uuid) = uuid::Uuid::parse_str(compact_id) {
            return self
                .library
                .query_row(
                    "SELECT id, media_id, path, season, episode, resolution, codec, hdr,
                            quality_source, confidence, filter_score
                     FROM ledger WHERE id = ?1",
                    params![uuid.to_string()],
                    map_ledger,
                )
                .optional()
                .map_err(Into::into);
        }
        // Fallback for non-standard compact ids: scan.
        Ok(self
            .list_ledger()?
            .into_iter()
            .find(|row| row.id.to_string().replace('-', "") == compact_id))
    }
}

#[cfg(test)]
mod batch_tests {
    use super::*;
    use domain::{Confidence, LedgerId, MediaId, QualitySource};

    fn seed_row(store: &Store, media_id: MediaId, index: usize) -> LedgerRow {
        let row = LedgerRow {
            id: LedgerId::new(),
            media_id,
            path: format!("/tmp/batch-{index}.mkv"),
            season: None,
            episode: None,
            resolution: None,
            codec: None,
            hdr: None,
            quality_source: QualitySource::Probe,
            confidence: Confidence::High,
            filter_score: None,
        };
        store.insert_ledger(&row).unwrap();
        row
    }

    #[test]
    fn ledger_for_media_batch_covers_chunk_boundary_and_dedups() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path()).unwrap();
        let ids: Vec<MediaId> = (0..501).map(|_| MediaId::new()).collect();
        for (index, media_id) in ids.iter().enumerate() {
            seed_row(&store, *media_id, index);
        }

        assert_eq!(store.ledger_for_media_batch(&ids).unwrap().len(), 501);
        assert!(store.ledger_for_media_batch(&[]).unwrap().is_empty());

        // 重复 id 只查询一次，不产生重复行。
        let mut duplicated = ids.clone();
        duplicated.extend(ids.iter().take(10).copied());
        assert_eq!(
            store.ledger_for_media_batch(&duplicated).unwrap().len(),
            501
        );
    }
}
