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
    /// The candidate set is a **superset** of everything `owner` can match: the
    /// caller still applies `owner` in memory, so over-reading is safe while a
    /// missed row would silently drop out of the projection. Rows under a deeper
    /// root of a nested child Library are still returned for the same reason.
    ///
    /// `ledger.path` keeps its historical raw spelling, so the SQL text cannot
    /// assume `path` is canonical. Each root contributes
    ///
    /// - its component-normalized text and its literal (trailing-separator
    ///   trimmed) text — both are *subsets* of `Path::starts_with(root)`, which
    ///   is what the default Library's `NOT (...)` fallback needs, and
    /// - two widened prefixes per root component boundary: the canonical text up
    ///   to that boundary followed by `//` or `/./`. A stored path that spells
    ///   the root portion non-canonically has a *first* boundary that is not a
    ///   single `/`; that boundary starts with `//` or `/./`, and the prefix
    ///   anchored just before it matches whatever follows. The union covers the
    ///   exact component-match set plus over-read rows the caller's `owner`
    ///   filter removes, so `/a/./b/x` and `/a//b/x` against a configured
    ///   `/a/b` are no longer dropped.
    ///
    /// Every Library needs the widened positive clause, the default one
    /// included: a same-kind **ancestor** root's narrow predicate matches a row
    /// spelled non-canonically *inside* the ancestor but outside the default's
    /// root text, which keeps that row out of the `NOT (...)` fallback. See
    /// `default_library_keeps_non_canonical_rows_blocked_by_an_ancestor_root`.
    ///
    /// Widening is option (b) of the residual fix: option (a), a maintained
    /// indexed component-normalized path fact, would live in the `ledger` DDL
    /// and the write/rename path, outside this module. The widening stays
    /// index-seekable instead of degrading into a scan;
    /// `non_default_candidate_read_stays_bounded_as_unrelated_ledger_grows`
    /// measures VM steps against a growing unrelated ledger.
    ///
    /// **Known limitation (P3-D):** for the kind's default Library the fallback
    /// branch is `NOT (matches any same-kind root)`, i.e. the complement of an
    /// indexed union. SQLite cannot seek that complement, so it scans the
    /// `ledger` b-tree: a default Library's build cost grows with total ledger
    /// rows, including unrelated ones, and the widened positive clause is
    /// evaluated per scanned row. Closing this needs an indexed "matches no
    /// root" fact (e.g. a maintained `root_id`); see the
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

        let is_default = ownership
            .default_for(kind)
            .is_some_and(|library| library.id == library_id);
        // 正向谓词一律用宽谓词，默认库也不能省：别的同 kind **祖先根**的窄谓词会先
        // 命中「祖先根之内、默认根之外」的非规范拼写，把它从下面的 `NOT (...)` 回退里
        // 挡掉，只有宽谓词能把它补回来。
        let target_roots: Vec<String> = target
            .root_paths
            .iter()
            .flat_map(|root| root_candidate_predicates(&mut values, root))
            .collect();
        if !target_roots.is_empty() {
            clauses.push(format!("({})", target_roots.join(" OR ")));
        }
        if is_default {
            // 未命中任何同 kind 根的行按 `owner` 回退到该 kind 的默认 Library。
            // 这里只能用**子集**谓词（`root_predicates`）：`NOT (...)` 必须保持
            // 「不命中任何根」的超集，带 `//`、`/./` 变体的宽谓词会把不属于任何
            // 根的行错误排除，默认库随之丢条目。
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

/// Separator spellings, other than a single `/`, that `Path::components()`
/// collapses at a component boundary: a repeated separator run and a `.`
/// segment. The first non-canonical boundary of a stored path therefore starts
/// with one of these two.
const DEVIANT_SEPARATORS: [&str; 2] = ["//", "/./"];

/// SQL predicates that are a **subset** of `Path::starts_with(root)`: the
/// component-normalized root text and the root's literal text. Only these may
/// feed the default Library's `NOT (...)` fallback, which must stay a superset
/// of "matches no root".
fn root_predicates(values: &mut Vec<rusqlite::types::Value>, root: &Path) -> Vec<String> {
    root_text_prefixes(root)
        .into_iter()
        .map(|prefix| prefix_predicate(values, &prefix))
        .collect()
}

/// SQL predicates that are a **superset** of `Path::starts_with(root)`: the
/// narrow ones plus one `//` and one `/./` variant anchored at every root
/// component boundary. A stored path is component-owned by the root either with
/// the root portion spelled canonically (narrow prefix) or with a first
/// non-canonical boundary starting `//` / `/./` (the anchored variant). Extra
/// rows are removed by `owner` in the caller, so this list must never feed the
/// `NOT (...)` fallback.
fn root_candidate_predicates(values: &mut Vec<rusqlite::types::Value>, root: &Path) -> Vec<String> {
    candidate_prefixes(root)
        .into_iter()
        .map(|prefix| prefix_predicate(values, &prefix))
        .collect()
}

/// Raw-text prefixes that are literally component-equivalent to the root.
fn root_text_prefixes(root: &Path) -> Vec<String> {
    let mut prefixes = vec![normalized_root_text(root)];
    let literal = root.to_string_lossy().trim_end_matches('/').to_string();
    if !prefixes.contains(&literal) {
        prefixes.push(literal);
    }
    prefixes
}

/// [`root_text_prefixes`] plus the boundary variants described on
/// [`root_candidate_predicates`].
fn candidate_prefixes(root: &Path) -> Vec<String> {
    let mut prefixes = root_text_prefixes(root);
    for boundary in boundary_prefixes(root) {
        for separator in DEVIANT_SEPARATORS {
            let variant = format!("{boundary}{separator}");
            if !prefixes.contains(&variant) {
                prefixes.push(variant);
            }
        }
    }
    prefixes
}

/// Canonical text of the first `i` normal components of the root, for every `i`
/// that precedes a component: index 0 is the leading separator run (`""`) and
/// index `i` is `/a` for `/a/b`. An absolute path's `RootDir` only contributes
/// the leading `/` of a non-empty prefix; interior `.` components are already
/// dropped by `Path::components()`.
fn boundary_prefixes(root: &Path) -> Vec<String> {
    let names: Vec<String> = root
        .components()
        .filter(|component| {
            matches!(
                component,
                std::path::Component::Normal(_) | std::path::Component::ParentDir
            )
        })
        .map(|component| component.as_os_str().to_string_lossy().to_string())
        .collect();
    let mut prefixes = Vec::with_capacity(names.len());
    for index in 0..names.len() {
        let mut prefix = String::new();
        if index > 0 && root.has_root() {
            prefix.push('/');
        }
        prefix.push_str(&names[..index].join("/"));
        prefixes.push(prefix);
    }
    prefixes
}

/// SQL predicate for "`path` is `prefix` or lives under it": the `prefix || '/'`
/// .. `prefix || '0'` range (byte-wise `'/' + 1 == '0'`) lets SQLite seek
/// `ledger_path_idx` instead of scanning.
///
/// A prefix that is empty or already ends in a separator (root `/`, or a
/// widened boundary variant) has no separate equality term: the half-open range
/// starts at the prefix itself. The empty prefix is root `/`, whose range is
/// every absolute path — the empty path is deliberately excluded because
/// `Path::new("").starts_with(Path::new("/"))` is false, so it must keep falling
/// back to the default Library.
fn prefix_predicate(values: &mut Vec<rusqlite::types::Value>, prefix: &str) -> String {
    let separator_terminated = prefix.is_empty() || prefix.ends_with('/');
    let base = prefix.strip_suffix('/').unwrap_or(prefix);
    values.push(rusqlite::types::Value::from(base.to_string()));
    let index = values.len();
    if separator_terminated {
        format!("(path >= ?{index} || '/' AND path < ?{index} || '0')")
    } else {
        format!("(path = ?{index} OR (path >= ?{index} || '/' AND path < ?{index} || '0'))")
    }
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
