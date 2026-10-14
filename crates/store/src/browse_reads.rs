//! Batch read facts for browse/playback consumers: media by id, and one
//! user's complete playback units.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

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
    ///
    /// The SQL predicate is text-based, while `owner` compares whole `Path`
    /// components. To stay equivalent for roots written as `/movies/./` or
    /// `/movies//`, every root contributes both its component-normalized text
    /// and its literal (trailing-separator-trimmed) text. Both are prefixes of
    /// the same component path, so each predicate is a *subset* of the true
    /// `Path::starts_with(root)` set — the union is still a subset, which keeps
    /// the default-library `NOT (...)` fallback a superset of "matches no root".
    /// A stored path that spells the root portion itself non-canonically (e.g.
    /// `/a/./b/x` against a configured `/a/b`) is the residual gap; the builder
    /// cannot see it because the text index never matches.
    ///
    /// **Known limitation (P3-D):** for the kind's default Library the fallback
    /// branch is `NOT (matches any same-kind root)`, i.e. the complement of an
    /// indexed union. SQLite cannot seek that complement, so it scans the
    /// `ledger` b-tree: a default Library's build cost grows with total ledger
    /// rows, including unrelated ones. Closing this needs an indexed
    /// "matches no root" fact (e.g. a maintained `root_id`); see the
    /// `default_library_candidate_read_is_not_bounded` test.
    pub fn ledger_candidates_for_library(
        &self,
        ownership: &LibraryOwnership,
        library_id: &str,
    ) -> Result<Vec<LedgerRow>, StoreError> {
        self.ledger_candidates_with_vm_steps(ownership, library_id)
            .map(|(rows, _)| rows)
    }

    /// Same read as [`Self::ledger_candidates_for_library`], additionally
    /// reporting the SQLite VM steps the candidate query consumed so tests can
    /// assert DB work instead of only returned rows.
    fn ledger_candidates_with_vm_steps(
        &self,
        ownership: &LibraryOwnership,
        library_id: &str,
    ) -> Result<(Vec<LedgerRow>, i64), StoreError> {
        let Some(target) = ownership.library(library_id) else {
            return Ok((Vec::new(), 0));
        };
        let kind = target.kind;
        let mut values: Vec<rusqlite::types::Value> = Vec::new();
        let mut clauses: Vec<String> = Vec::new();

        let target_roots: Vec<String> = target
            .root_paths
            .iter()
            .flat_map(|root| root_predicates(&mut values, root))
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
                    all_roots.extend(root_predicates(&mut values, root));
                }
            }
            if all_roots.is_empty() {
                clauses.push("1".to_string());
            } else {
                clauses.push(format!("NOT ({})", all_roots.join(" OR ")));
            }
        }

        if clauses.is_empty() {
            return Ok((Vec::new(), 0));
        }
        let sql = format!(
            "SELECT id, media_id, path, season, episode, resolution, codec, hdr,
                    quality_source, confidence, filter_score
             FROM ledger WHERE {}",
            clauses.join(" OR ")
        );
        let mut stmt = self.library.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(values), map_ledger)?;
        let collected = rows.collect::<Result<Vec<_>, _>>()?;
        let vm_steps = i64::from(stmt.get_status(rusqlite::StatementStatus::VmStep));
        Ok((collected, vm_steps))
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

/// SQL predicates equivalent to `Path::starts_with(root)` for the `ledger.path`
/// text column: equal, or inside the half-open range `prefix || '/'` ..
/// `prefix || '0'` (byte-wise `'/' + 1 == '0'`), which is exactly the strings
/// sharing the prefix plus one separator and lets SQLite use `ledger_path_idx`
/// instead of scanning unrelated rows.
///
/// `root` is emitted once for its component-normalized text (`Path::components`
/// drops `.` and repeated separators) and once for its literal text with
/// trailing separators trimmed. Any string matching either prefix starts with
/// the root's components, so both are subsets of the true `starts_with` set and
/// the OR stays safe for the default-library `NOT (...)` fallback.
fn root_predicates(values: &mut Vec<rusqlite::types::Value>, root: &Path) -> Vec<String> {
    let normalized = normalized_root_text(root);
    let literal = root.to_string_lossy().trim_end_matches('/').to_string();
    let mut prefixes = vec![normalized];
    if prefixes[0] != literal {
        prefixes.push(literal);
    }
    prefixes
        .into_iter()
        .map(|prefix| {
            values.push(rusqlite::types::Value::from(prefix));
            let index = values.len();
            format!("(path = ?{index} OR (path >= ?{index} || '/' AND path < ?{index} || '0'))")
        })
        .collect()
}

/// `root` with `.` components and repeated separators removed, matching the
/// component view `Path::starts_with` compares. Trailing separators are trimmed
/// so `/movies/` and `/movies` share one predicate; the root `/` trims to the
/// empty prefix, whose range `>= '/' AND < '0'` is every absolute path.
fn normalized_root_text(root: &Path) -> String {
    let mut normalized = PathBuf::new();
    for component in root.components() {
        normalized.push(component.as_os_str());
    }
    normalized.to_string_lossy().trim_end_matches('/').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::{Confidence, LedgerId, MediaKind, QualitySource};

    fn insert_row(store: &Store, path: &Path) {
        store
            .insert_ledger(&LedgerRow {
                id: LedgerId::new(),
                media_id: MediaId::new(),
                path: path.to_string_lossy().to_string(),
                season: None,
                episode: None,
                resolution: None,
                codec: None,
                hdr: None,
                quality_source: QualitySource::Release,
                confidence: Confidence::High,
                filter_score: None,
            })
            .unwrap();
    }

    /// P3-D：非默认库的候选读走 `ledger_path_idx`，VM steps 不随无关台账增长。
    #[test]
    fn non_default_candidate_read_stays_bounded_as_unrelated_ledger_grows() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path().join("data")).unwrap();
        let target_root = tmp.path().join("movies");
        let other_root = tmp.path().join("other");
        std::fs::create_dir_all(&target_root).unwrap();
        std::fs::create_dir_all(&other_root).unwrap();
        let target = store
            .create_library(
                MediaKind::Movie,
                "Target",
                &[target_root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        store
            .create_library(
                MediaKind::Movie,
                "Other",
                &[other_root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        for index in 0..3 {
            insert_row(&store, &target_root.join(format!("m{index}.mkv")));
        }
        for index in 0..10 {
            insert_row(&store, &other_root.join(format!("o{index}.mkv")));
        }
        let ownership = store.library_ownership().unwrap();
        let (small_rows, small_steps) = store
            .ledger_candidates_with_vm_steps(&ownership, &target.id)
            .unwrap();

        for index in 10..3_000 {
            insert_row(&store, &other_root.join(format!("o{index}.mkv")));
        }
        let (large_rows, large_steps) = store
            .ledger_candidates_with_vm_steps(&ownership, &target.id)
            .unwrap();

        assert_eq!(small_rows.len(), 3);
        assert_eq!(large_rows.len(), 3);
        assert!(small_steps > 0, "候选读必须真的执行了 SQL");
        assert!(
            large_steps <= small_steps + 100,
            "非默认库候选读必须仍用索引：无关台账 10→3000 行时 VM steps {small_steps}→{large_steps}"
        );
    }

    /// P3-D 已记录的限制：默认库回退分支是「不属于任何同 kind 根」的补集，
    /// SQLite 无法 seek，VM steps 随整个 ledger 增长（返回行数却不变）。
    #[test]
    fn default_library_candidate_read_is_not_bounded() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path().join("data")).unwrap();
        let other_root = tmp.path().join("other");
        std::fs::create_dir_all(&other_root).unwrap();
        store
            .create_library(
                MediaKind::Movie,
                "Other",
                &[other_root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        let default = store
            .default_library(MediaKind::Movie)
            .unwrap()
            .expect("seeded default movie library");
        insert_row(&store, &tmp.path().join("stray").join("x.mkv"));
        for index in 0..10 {
            insert_row(&store, &other_root.join(format!("o{index}.mkv")));
        }
        let ownership = store.library_ownership().unwrap();
        let (small_rows, small_steps) = store
            .ledger_candidates_with_vm_steps(&ownership, &default.id)
            .unwrap();

        for index in 10..3_000 {
            insert_row(&store, &other_root.join(format!("o{index}.mkv")));
        }
        let (large_rows, large_steps) = store
            .ledger_candidates_with_vm_steps(&ownership, &default.id)
            .unwrap();

        assert_eq!(small_rows.len(), large_rows.len());
        assert_eq!(small_rows.len(), 1, "无关根的行不进入默认库候选");
        assert!(
            large_steps > small_steps + 1_000,
            "默认库回退必须如实暴露全表扫描：VM steps {small_steps}→{large_steps}"
        );
    }

    /// `./`、重复分隔符与尾部分隔符在组件层面等价于规范根。
    #[test]
    fn root_predicates_normalize_dot_and_repeated_separators() {
        assert_eq!(normalized_root_text(Path::new("/a/b/./")), "/a/b");
        assert_eq!(normalized_root_text(Path::new("/a//b//")), "/a/b");
        assert_eq!(normalized_root_text(Path::new("/a/b/")), "/a/b");
        assert_eq!(normalized_root_text(Path::new("/")), "");
    }
}

