//! Batch read facts for browse/playback consumers: media by id, and one
//! user's complete playback units.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use domain::{LedgerRow, Media, MediaId, UserId};
use rusqlite::params;

use super::maps::{map_ledger, map_media, parse_id};
use super::playback::UnitState;
use super::schema::MEDIA_COLS;
use super::{LibraryOwnership, Store, StoreError};

/// One playback unit with its media id and full per-unit state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaybackUnitSnapshot {
    pub media_id: MediaId,
    pub season: i32,
    pub episode: i32,
    pub state: UnitState,
}

const MEDIA_BATCH_CHUNK: usize = 500;

impl Store {
    /// Media rows for `ids`; unknown ids are simply absent, and empty input is
    /// an empty map. Ids are chunked to stay under SQLite's variable limit.
    pub fn media_batch(&self, ids: &[MediaId]) -> Result<HashMap<MediaId, Media>, StoreError> {
        let mut found = HashMap::new();
        for chunk in ids.chunks(MEDIA_BATCH_CHUNK) {
            let placeholders = vec!["?"; chunk.len()].join(", ");
            let sql = format!("SELECT {MEDIA_COLS} FROM media WHERE id IN ({placeholders})");
            let mut stmt = self.app.prepare(&sql)?;
            let rows = stmt.query_map(
                rusqlite::params_from_iter(chunk.iter().map(MediaId::to_string)),
                map_media,
            )?;
            for row in rows {
                let media = row?;
                found.insert(media.id, media);
            }
        }
        Ok(found)
    }

    /// Every playback unit for one user, with the same columns as
    /// `Store::unit_state` (position, play count, and track preferences
    /// included — not a reduced season/episode/played row).
    pub fn playback_units_for_user(
        &self,
        user: UserId,
    ) -> Result<Vec<PlaybackUnitSnapshot>, StoreError> {
        let mut stmt = self.subscribe.prepare(
            "SELECT media_id, season, episode,
                    position_ms, played, favorite, duration_ms, audio_track,
                    subtitle_track, play_count, updated_at
             FROM playback_units WHERE user_id = ?1
             ORDER BY media_id, season, episode",
        )?;
        let rows = stmt.query_map(params![user.to_string()], |row| {
            Ok(PlaybackUnitSnapshot {
                media_id: parse_id(row.get::<_, String>(0)?, 0)?,
                season: row.get(1)?,
                episode: row.get(2)?,
                state: UnitState {
                    position_ms: row.get(3)?,
                    played: row.get::<_, i64>(4)? != 0,
                    favorite: row.get::<_, i64>(5)? != 0,
                    duration_ms: row.get(6)?,
                    audio_track: row.get(7)?,
                    subtitle_track: row.get(8)?,
                    play_count: row.get(9)?,
                    updated_at: row.get(10)?,
                },
            })
        })?;
        let mut units = Vec::new();
        for row in rows {
            units.push(row?);
        }
        Ok(units)
    }

    /// Playback units for a specific list of media ids, chunked to stay under SQLite variable limits.
    pub fn playback_units_for_media(
        &self,
        user: UserId,
        ids: &[MediaId],
    ) -> Result<Vec<PlaybackUnitSnapshot>, StoreError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut units = Vec::new();
        for chunk in ids.chunks(MEDIA_BATCH_CHUNK) {
            let placeholders = vec!["?"; chunk.len()].join(", ");
            let sql = format!(
                "SELECT media_id, season, episode,
                        position_ms, played, favorite, duration_ms, audio_track,
                        subtitle_track, play_count, updated_at
                 FROM playback_units
                 WHERE user_id = ?1 AND media_id IN ({placeholders})
                 ORDER BY media_id, season, episode"
            );
            let mut stmt = self.subscribe.prepare(&sql)?;
            let mut params_vec = Vec::with_capacity(1 + chunk.len());
            params_vec.push(rusqlite::types::Value::from(user.to_string()));
            for id in chunk {
                params_vec.push(rusqlite::types::Value::from(id.to_string()));
            }
            let rows = stmt.query_map(rusqlite::params_from_iter(params_vec), |row| {
                Ok(PlaybackUnitSnapshot {
                    media_id: parse_id(row.get::<_, String>(0)?, 0)?,
                    season: row.get(1)?,
                    episode: row.get(2)?,
                    state: UnitState {
                        position_ms: row.get(3)?,
                        played: row.get::<_, i64>(4)? != 0,
                        favorite: row.get::<_, i64>(5)? != 0,
                        duration_ms: row.get(6)?,
                        audio_track: row.get(7)?,
                        subtitle_track: row.get(8)?,
                        play_count: row.get(9)?,
                        updated_at: row.get(10)?,
                    },
                })
            })?;
            for row in rows {
                units.push(row?);
            }
        }
        Ok(units)
    }

    /// Page browse_media rows directly from the projection for simple title queries.
    pub fn browse_title_page(
        &self,
        library_id: &str,
        offset: usize,
        limit: usize,
        desc: bool,
    ) -> Result<Vec<super::BrowseMediaFacts>, StoreError> {
        let order = if desc { "DESC" } else { "ASC" };
        let sql = format!(
            "SELECT media_id, title, year, facts_json
             FROM browse_media
             WHERE library_id = ?1
             ORDER BY title {order}, media_id ASC
             LIMIT ?2 OFFSET ?3"
        );
        let mut stmt = self.library.prepare(&sql)?;
        let rows = stmt.query_map(params![library_id, limit, offset], |row| {
            let mid: String = row.get(0)?;
            let title: String = row.get(1)?;
            let year: Option<u16> = row.get(2)?;
            let facts_str: String = row.get(3)?;
            use std::str::FromStr;
            let media_id = MediaId::from_str(&mid)
                .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, 0))?;
            let facts = serde_json::from_str(&facts_str).unwrap_or_default();
            Ok(super::BrowseMediaFacts {
                media_id,
                title,
                year,
                facts,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Candidate ledger rows for one Library under the historical ownership
    /// rule used by [`LibraryOwnership::owner`] (longest same-kind root wins; a
    /// complete miss falls back to the kind's default Library).
    ///
    /// This is a candidate **superset**: rows under a deeper root of a nested
    /// child Library are still returned so the caller can apply `owner`, but
    /// rows under unrelated roots are never read. That keeps the projection
    /// builder's DB rows, lock hold, and memory bounded by the target Library
    /// instead of the whole instance.
    pub fn ledger_candidates_for_library(
        &self,
        ownership: &LibraryOwnership,
        library_id: &str,
    ) -> Result<Vec<LedgerRow>, StoreError> {
        let Some(target) = ownership.library(library_id) else {
            return Ok(Vec::new());
        };
        let kind = target.kind;
        let mut values: Vec<rusqlite::types::Value> = Vec::new();
        let mut clauses: Vec<String> = Vec::new();

        let target_roots: Vec<String> = target
            .root_paths
            .iter()
            .map(|root| root_predicate(&mut values, root))
            .collect();
        if !target_roots.is_empty() {
            clauses.push(format!("({})", target_roots.join(" OR ")));
        }

        let is_default = ownership
            .default_for(kind)
            .is_some_and(|library| library.id == library_id);
        if is_default {
            // 未命中任何同 kind 根的行按 `owner` 回退到该 kind 的默认 Library。
            let mut all_roots: Vec<String> = Vec::new();
            for library in ownership.libraries().iter().filter(|lib| lib.kind == kind) {
                for root in &library.root_paths {
                    all_roots.push(root_predicate(&mut values, root));
                }
            }
            if all_roots.is_empty() {
                clauses.push("1".to_string());
            } else {
                clauses.push(format!("NOT ({})", all_roots.join(" OR ")));
            }
        }

        if clauses.is_empty() {
            return Ok(Vec::new());
        }
        let sql = format!(
            "SELECT id, media_id, path, season, episode, resolution, codec, hdr,
                    quality_source, confidence, filter_score
             FROM ledger WHERE {}",
            clauses.join(" OR ")
        );
        let mut stmt = self.library.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(values), map_ledger)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Missing-file markers for the given ledger paths only, chunked under the
    /// SQLite variable limit. Unrelated missing rows are not read, so a growing
    /// instance ledger does not grow this read.
    pub fn missing_at_for_paths(&self, paths: &[String]) -> Result<HashSet<String>, StoreError> {
        let unique: HashSet<&str> = paths.iter().map(String::as_str).collect();
        let unique: Vec<&str> = unique.into_iter().collect();
        let mut missing = HashSet::new();
        for chunk in unique.chunks(MEDIA_BATCH_CHUNK) {
            let placeholders = vec!["?"; chunk.len()].join(", ");
            let sql = format!(
                "SELECT path FROM ledger
                 WHERE missing_at IS NOT NULL AND path IN ({placeholders})"
            );
            let mut stmt = self.library.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter().copied()), |row| {
                row.get::<_, String>(0)
            })?;
            for row in rows {
                missing.insert(row?);
            }
        }
        Ok(missing)
    }
}

/// SQL predicate equivalent to `Path::starts_with(root)` for the `ledger.path`
/// text column: equal, or inside the half-open range `root || '/'` ..
/// `root || '0'` (byte-wise `'/' + 1 == '0'`), which is exactly the strings
/// sharing the root plus one separator and lets SQLite use `ledger_path_idx`
/// instead of scanning unrelated rows. Roots are normalized by trimming
/// trailing separators, so `/movies` and `/movies/` behave identically and
/// `/movies-other` does not match `/movies`.
fn root_predicate(values: &mut Vec<rusqlite::types::Value>, root: &Path) -> String {
    let index = values.len() + 1;
    let normalized = root.to_string_lossy();
    values.push(rusqlite::types::Value::from(
        normalized.trim_end_matches('/').to_string(),
    ));
    format!("(path = ?{index} OR (path >= ?{index} || '/' AND path < ?{index} || '0'))")
}
