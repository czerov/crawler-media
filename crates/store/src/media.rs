use domain::{Media, MediaId};
use rusqlite::{OptionalExtension, params};

use super::maps::map_media;
use super::schema::MEDIA_COLS;
use super::{Store, StoreError};

fn aliases(media: &Media) -> [Option<&str>; 5] {
    [
        media.tmdb_id.as_deref(),
        media.douban_id.as_deref(),
        media.tvdb_id.as_deref(),
        media.bangumi_id.as_deref(),
        media.anilist_id.as_deref(),
    ]
}

fn aliases_conflict(a: &Media, b: &Media) -> bool {
    aliases(a)
        .into_iter()
        .zip(aliases(b))
        .any(|(a, b)| a.zip(b).is_some_and(|(a, b)| a != b))
}

fn identity_conflict(incoming: &Media) -> StoreError {
    tracing::warn!(media_id = %incoming.id, kind = incoming.kind.as_str(), "Ambiguous scanned Media identity rejected");
    StoreError::Protected("Ambiguous or conflicting scanned Media identity".into())
}

impl Store {
    pub fn insert_media(&self, media: &Media) -> Result<(), StoreError> {
        self.app.execute(
            "INSERT INTO media (id, kind, title, year, original_title, tmdb_id, douban_id, tvdb_id, bangumi_id, anilist_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                media.id.to_string(),
                media.kind.as_str(),
                media.title,
                media.year.map(i64::from),
                media.original_title,
                media.tmdb_id,
                media.douban_id,
                media.tvdb_id,
                media.bangumi_id,
                media.anilist_id,
            ],
        )?;
        Ok(())
    }

    /// Safely undo a newly inserted media if it matches expected and is completely unreferenced by subscribes or ledgers.
    pub fn undo_ensured_media_if_unreferenced(
        &self,
        media_id: MediaId,
        expected: &Media,
    ) -> Result<bool, StoreError> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.app,
            rusqlite::TransactionBehavior::Immediate,
        )?;

        let id_str = media_id.to_string();
        let matches_expected: bool = tx.query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM media
                 WHERE id = ?1 AND title = ?2 AND kind = ?3
                   AND year IS ?4 AND original_title IS ?5
                   AND tmdb_id IS ?6 AND douban_id IS ?7
                   AND tvdb_id IS ?8 AND bangumi_id IS ?9
                   AND anilist_id IS ?10
             )",
            params![
                id_str,
                expected.title,
                expected.kind.as_str(),
                expected.year,
                expected.original_title,
                expected.tmdb_id,
                expected.douban_id,
                expected.tvdb_id,
                expected.bangumi_id,
                expected.anilist_id,
            ],
            |r| r.get(0),
        )?;
        if !matches_expected {
            return Ok(false);
        }

        let has_sub: bool = self.subscribe.query_row(
            "SELECT EXISTS(SELECT 1 FROM subscribes WHERE media_id = ?1)",
            [&id_str],
            |r| r.get(0),
        )?;
        if has_sub {
            return Ok(false);
        }

        let has_ledger: bool = self.library.query_row(
            "SELECT EXISTS(SELECT 1 FROM ledger WHERE media_id = ?1)",
            [&id_str],
            |r| r.get(0),
        )?;
        if has_ledger {
            return Ok(false);
        }

        let changed = tx.execute("DELETE FROM media WHERE id = ?1", [&id_str])?;
        tx.commit()?;
        Ok(changed > 0)
    }

    pub fn get_media(&self, id: MediaId) -> Result<Option<Media>, StoreError> {
        self.app
            .query_row(
                &format!("SELECT {MEDIA_COLS} FROM media WHERE id = ?1"),
                params![id.to_string()],
                map_media,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn get_media_by_title(&self, title: &str) -> Result<Option<Media>, StoreError> {
        self.app
            .query_row(
                &format!("SELECT {MEDIA_COLS} FROM media WHERE title = ?1"),
                params![title],
                map_media,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn get_media_by_title_kind_year(
        &self,
        title: &str,
        kind: domain::MediaKind,
        year: Option<u16>,
    ) -> Result<Option<Media>, StoreError> {
        let year_i64 = year.map(i64::from);
        // year 有值：精确匹配年份。
        if year_i64.is_some() {
            return self
                .app
                .query_row(
                    &format!(
                        "SELECT {MEDIA_COLS} FROM media
                         WHERE title = ?1 AND kind = ?2 AND year IS ?3 LIMIT 1"
                    ),
                    params![title, kind.as_str(), year_i64],
                    map_media,
                )
                .optional()
                .map_err(Into::into);
        }
        // year 为 None（文件名未带年份）：
        // 1) 优先匹配同样「未知年份」的行（year IS NULL）；
        // 2) 否则仅当恰好一个「已知年份」候选时挂它（同名不同年份并存时
        //    无法区分，返回 None 不任取，避免把无年份文件挂错作品）。
        if let Some(media) = self
            .app
            .query_row(
                &format!(
                    "SELECT {MEDIA_COLS} FROM media
                     WHERE title = ?1 AND kind = ?2 AND year IS NULL LIMIT 1"
                ),
                params![title, kind.as_str()],
                map_media,
            )
            .optional()?
        {
            return Ok(Some(media));
        }
        let mut stmt = self.app.prepare(&format!(
            "SELECT {MEDIA_COLS} FROM media WHERE title = ?1 AND kind = ?2 AND year IS NOT NULL"
        ))?;
        let rows = stmt
            .query_map(params![title, kind.as_str()], map_media)?
            .collect::<Result<Vec<_>, _>>()?;
        if rows.len() == 1 {
            Ok(rows.into_iter().next())
        } else {
            Ok(None)
        }
    }

    pub fn get_media_by_tmdb(&self, tmdb_id: &str) -> Result<Option<Media>, StoreError> {
        self.get_media_by_alias("tmdb_id", tmdb_id)
    }

    pub fn get_media_by_alias(&self, column: &str, id: &str) -> Result<Option<Media>, StoreError> {
        self.get_media_by_alias_kind(column, id, None)
    }

    /// 按别名 + kind 查找（ensure_media 用）：相同数字的 TMDB movie/TV id
    /// 不会被合并成同一个内部 Media。
    pub fn get_media_by_alias_kind(
        &self,
        column: &str,
        id: &str,
        kind: Option<domain::MediaKind>,
    ) -> Result<Option<Media>, StoreError> {
        if !matches!(
            column,
            "tmdb_id" | "douban_id" | "tvdb_id" | "bangumi_id" | "anilist_id"
        ) {
            return Ok(None);
        }
        let sql = match kind {
            Some(_kind) => {
                format!("SELECT {MEDIA_COLS} FROM media WHERE {column} = ?1 AND kind = ?2")
            }
            None => format!("SELECT {MEDIA_COLS} FROM media WHERE {column} = ?1"),
        };
        let row = match kind {
            Some(kind) => self
                .app
                .query_row(&sql, params![id, kind.as_str()], map_media)
                .optional()?,
            None => self
                .app
                .query_row(&sql, params![id], map_media)
                .optional()?,
        };
        Ok(row)
    }

    pub fn update_media(&self, media: &Media) -> Result<(), StoreError> {
        self.app.execute(
            "UPDATE media SET title = ?2, year = ?3, original_title = ?4, tmdb_id = ?5,
                    douban_id = ?6, tvdb_id = ?7, bangumi_id = ?8, anilist_id = ?9 WHERE id = ?1",
            params![
                media.id.to_string(),
                media.title,
                media.year.map(i64::from),
                media.original_title,
                media.tmdb_id,
                media.douban_id,
                media.tvdb_id,
                media.bangumi_id,
                media.anilist_id,
            ],
        )?;
        Ok(())
    }

    /// Recover canonical identity without selecting the first alias or ambiguous title.
    pub fn resolve_scanned_media(&self, incoming: Media) -> Result<Media, StoreError> {
        let mut stmt = self.app.prepare(&format!(
            "SELECT {MEDIA_COLS} FROM media WHERE kind = ?1 AND
             (title = ?2 OR tmdb_id = ?3 OR douban_id = ?4 OR tvdb_id = ?5
              OR bangumi_id = ?6 OR anilist_id = ?7)"
        ))?;
        let candidates = stmt
            .query_map(
                params![
                    incoming.kind.as_str(),
                    incoming.title,
                    incoming.tmdb_id,
                    incoming.douban_id,
                    incoming.tvdb_id,
                    incoming.bangumi_id,
                    incoming.anilist_id
                ],
                map_media,
            )?
            .collect::<Result<Vec<_>, _>>()?;
        let external: Vec<_> = candidates
            .iter()
            .filter(|existing| {
                aliases(existing)
                    .into_iter()
                    .zip(aliases(&incoming))
                    .any(|(a, b)| a.zip(b).is_some_and(|(a, b)| a == b))
            })
            .collect();
        let has_incoming_alias = aliases(&incoming).iter().any(Option::is_some);
        let existing = if external.len() > 1 {
            return Err(identity_conflict(&incoming));
        } else if let Some(existing) = external.first() {
            if aliases_conflict(existing, &incoming) {
                return Err(identity_conflict(&incoming));
            }
            Some(*existing)
        } else if let Some(same_id) = candidates.iter().find(|existing| existing.id == incoming.id) {
            Some(same_id)
        } else if !has_incoming_alias {
            let titles: Vec<_> = candidates
                .iter()
                .filter(|existing| {
                    existing.title == incoming.title
                        && incoming.year == existing.year
                        && !aliases_conflict(existing, &incoming)
                })
                .collect();
            if titles.len() > 1 {
                return Err(identity_conflict(&incoming));
            }
            titles.first().copied()
        } else {
            None
        };
        if let Some(existing) = existing {
            let mut canonical = existing.clone();
            canonical.year = canonical.year.or(incoming.year);
            canonical.original_title = canonical.original_title.or(incoming.original_title);
            canonical.tmdb_id = canonical.tmdb_id.or(incoming.tmdb_id);
            canonical.douban_id = canonical.douban_id.or(incoming.douban_id);
            canonical.tvdb_id = canonical.tvdb_id.or(incoming.tvdb_id);
            canonical.bangumi_id = canonical.bangumi_id.or(incoming.bangumi_id);
            canonical.anilist_id = canonical.anilist_id.or(incoming.anilist_id);
            self.update_media(&canonical)?;
            tracing::debug!(media_id = %canonical.id, "Recovered canonical scanned Media identity");
            return Ok(canonical);
        }
        self.insert_media(&incoming)?;
        Ok(incoming)
    }

    pub fn ensure_media(&self, incoming: Media) -> Result<Media, StoreError> {
        self.ensure_media_with_previous(incoming)
            .map(|(media, _)| media)
    }

    /// Return the original row so a multi-database Subscribe creation can undo
    /// both a new Media and an alias merge if a later write fails.
    pub fn ensure_media_with_previous(
        &self,
        incoming: Media,
    ) -> Result<(Media, Option<Media>), StoreError> {
        let aliases = [
            ("tmdb_id", incoming.tmdb_id.clone()),
            ("douban_id", incoming.douban_id.clone()),
            ("tvdb_id", incoming.tvdb_id.clone()),
            ("bangumi_id", incoming.bangumi_id.clone()),
            ("anilist_id", incoming.anilist_id.clone()),
        ];
        for (column, id) in aliases {
            let Some(id) = id else { continue };
            // 别名查找必须带上 kind：TMDB 的 movie/tv 共用数字 id 空间，
            // 不带 kind 会把电影和剧集合并成同一个内部 Media。
            if let Some(existing) =
                self.get_media_by_alias_kind(column, &id, Some(incoming.kind))?
            {
                let merged = ::media::merge(existing.clone(), incoming);
                self.update_media(&merged)?;
                return Ok((merged, Some(existing)));
            }
        }
        self.insert_media(&incoming)?;
        Ok((incoming, None))
    }

    pub fn undo_ensured_media(
        &self,
        media: &Media,
        previous: Option<&Media>,
    ) -> Result<bool, StoreError> {
        if let Some(previous) = previous {
            let tx = rusqlite::Transaction::new_unchecked(
                &self.app,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            let current_matches: bool = tx.query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM media
                     WHERE id = ?1 AND title = ?2 AND kind = ?3
                       AND year IS ?4 AND original_title IS ?5
                       AND tmdb_id IS ?6 AND douban_id IS ?7
                       AND tvdb_id IS ?8 AND bangumi_id IS ?9
                       AND anilist_id IS ?10
                 )",
                params![
                    media.id.to_string(),
                    media.title,
                    media.kind.as_str(),
                    media.year,
                    media.original_title,
                    media.tmdb_id,
                    media.douban_id,
                    media.tvdb_id,
                    media.bangumi_id,
                    media.anilist_id,
                ],
                |r| r.get(0),
            )?;
            if !current_matches {
                return Ok(false);
            }
            tx.execute(
                "UPDATE media SET title = ?2, year = ?3, original_title = ?4, tmdb_id = ?5,
                 douban_id = ?6, tvdb_id = ?7, bangumi_id = ?8, anilist_id = ?9
                 WHERE id = ?1",
                params![
                    previous.id.to_string(),
                    previous.title,
                    previous.year,
                    previous.original_title,
                    previous.tmdb_id,
                    previous.douban_id,
                    previous.tvdb_id,
                    previous.bangumi_id,
                    previous.anilist_id,
                ],
            )?;
            tx.commit()?;
            Ok(true)
        } else {
            self.undo_ensured_media_if_unreferenced(media.id, media)
        }
    }

    pub fn delete_media(&self, id: MediaId) -> Result<(), StoreError> {
        self.app
            .execute("DELETE FROM media WHERE id = ?1", params![id.to_string()])?;
        Ok(())
    }
}
