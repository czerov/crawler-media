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

/// 一个 Library 的投影 file facts 汇总（一次 SQL 聚合，零 JSON 解码）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BrowseFileTotals {
    /// `browse_files` 行数：该库当前拥有的台账文件数。
    pub file_count: u64,
    /// 物理体积合计；`.strm` 指针文件记 0（与 Library 统计既有口径一致）。
    pub total_size_bytes: u64,
    /// `.strm` 指针文件数。
    pub strm_file_count: u64,
}

pub(super) const MEDIA_BATCH_CHUNK: usize = 500;

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
                    subtitle_track, play_count, updated_at, favorite_at
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
                    favorite_at: row.get(11)?,
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
                        subtitle_track, play_count, updated_at, favorite_at
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
                        favorite_at: row.get(11)?,
                    },
                })
            })?;
            for row in rows {
                units.push(row?);
            }
        }
        Ok(units)
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


    /// Restrict a Jellyfin parent request to ownership candidates before loading
    /// media and playback facts.
    pub fn jellyfin_ledger_candidates(
        &self,
        ownership: &LibraryOwnership,
        parent_id: Option<&str>,
    ) -> Result<Vec<LedgerRow>, StoreError> {
        match parent_id {
            Some(id) if ownership.library(id).is_some() => self.ledger_candidates_for_library(ownership, id),
            Some(id) => match id.parse::<MediaId>() {
                Ok(media_id) => self.ledger_for_media(media_id),
                Err(_) => Ok(Vec::new()),
            },
            None => self.list_ledger(),
        }
    }

    pub fn jellyfin_ledger_candidates_with_vm_steps(
        &self,
        ownership: &LibraryOwnership,
        parent_id: &str,
    ) -> Result<(Vec<LedgerRow>, i64), StoreError> {
        self.ledger_candidates_with_vm_steps(ownership, parent_id)
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

    /// 一个 Library 的投影 file facts 汇总：行数、物理体积、`.strm` 指针数。
    ///
    /// 一次 SQL 聚合，不解码整库 facts JSON，也不碰文件系统；体积口径与旧逐文件
    /// `stat` 一致（`.strm` 只有几十字节 URL 文本，记 0）。任何一行的 facts JSON
    /// 不是 `facts_version`（升级前的旧代或损坏行）时返回 `None`：调用方据此判定
    /// 投影过旧并重建，而不是把 0 当成「空库」。
    pub fn browse_file_totals(
        &self,
        library_id: &str,
        facts_version: u64,
    ) -> Result<Option<BrowseFileTotals>, StoreError> {
        // A5-01: TOTAL() 浮点聚合避免 SQLite 整数加法溢出。
        // 每一行 facts_json 必须通过严格的有效性验证：
        // - json_valid(facts_json)
        // - json_extract(facts_json, '$.version') = ?2
        // - json_type(facts_json, '$.size') IN ('integer', 'real')
        // - json_extract(facts_json, '$.size') >= 0 且 <= 100_000_000_000_000_000 (100 PB)
        // 任何损坏/非法/负数/越界 size 的行都记入 foreign 计数，使该库 facts 标记不可用 (None) 等待重建，
        // 而不是静默吞掉负数或让整个请求 500 崩溃。
        const MAX_SANE_FILE_SIZE: f64 = 100_000_000_000_000_000.0; // 100 PB
        let mut stmt = self.library.prepare(
            "SELECT COUNT(*),
                    COALESCE(SUM(CASE WHEN json_valid(facts_json)
                                       AND json_extract(facts_json, '$.version') = ?2
                                       AND json_type(facts_json, '$.size') IN ('integer', 'real')
                                       AND json_extract(facts_json, '$.size') >= 0
                                       AND json_extract(facts_json, '$.size') <= ?3
                                      THEN 0 ELSE 1 END), 0),
                    COALESCE(SUM(CASE WHEN json_valid(facts_json)
                                       AND json_extract(facts_json, '$.version') = ?2
                                       AND lower(COALESCE(json_extract(facts_json, '$.path'), ''))
                                           LIKE '%.strm'
                                      THEN 1 ELSE 0 END), 0),
                    TOTAL(CASE WHEN json_valid(facts_json)
                                    AND json_extract(facts_json, '$.version') = ?2
                                    AND lower(COALESCE(json_extract(facts_json, '$.path'), ''))
                                        NOT LIKE '%.strm'
                                    AND json_type(facts_json, '$.size') IN ('integer', 'real')
                                    AND json_extract(facts_json, '$.size') >= 0
                                    AND json_extract(facts_json, '$.size') <= ?3
                               THEN json_extract(facts_json, '$.size')
                               ELSE 0.0 END)
             FROM browse_files WHERE library_id = ?1",
        )?;
        let (file_count, foreign, strm, size_float): (i64, i64, i64, f64) = stmt.query_row(
            params![library_id, facts_version as i64, MAX_SANE_FILE_SIZE],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
        if foreign > 0 || !size_float.is_finite() || size_float < 0.0 {
            return Ok(None);
        }
        let total_size_bytes = if size_float >= (u64::MAX as f64) {
            u64::MAX
        } else {
            size_float.round() as u64
        };
        Ok(Some(BrowseFileTotals {
            file_count: file_count.max(0) as u64,
            total_size_bytes,
            strm_file_count: strm.max(0) as u64,
        }))
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

    /// 整库 facts 汇总：一次 SQL 聚合、`.strm` 记 0、版本不符返回 `None`。
    #[test]
    fn browse_file_totals_zeroes_strm_and_rejects_old_facts() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path().join("data")).unwrap();
        let library = store
            .default_library(MediaKind::Movie)
            .unwrap()
            .expect("seeded default movie library");
        let media_id = MediaId::new();
        let file = |path: &str, size: u64| crate::BrowseFileFacts {
            ledger_id: LedgerId::new(),
            media_id,
            path: path.to_owned(),
            facts: serde_json::json!({
                "version": 3,
                "path": path,
                "size": size,
                "missing": false,
            }),
        };
        let scope = format!(
            "{}|{}",
            library.kind.as_str(),
            library
                .root_paths
                .iter()
                .map(|root| root.display().to_string())
                .collect::<Vec<_>>()
                .join("\u{1f}")
        );
        let build = crate::ProjectionBuild {
            library_id: library.id.clone(),
            generation: 1,
            scope_signature: scope,
            revisions: store.read_revisions().unwrap(),
            expected_dirty_token: None,
            files: vec![file("/movies/a.mkv", 10), file("/movies/b.strm", 7)],
            media: Vec::new(),
            candidates: Vec::new(),
        };
        assert!(store.publish_browse_projection(&build).unwrap());

        let totals = store
            .browse_file_totals(&library.id, 3)
            .unwrap()
            .expect("current facts");
        assert_eq!(totals.file_count, 2);
        assert_eq!(totals.total_size_bytes, 10, ".strm 指针不计物理体积");
        assert_eq!(totals.strm_file_count, 1);
        // 旧代 facts 版本：不能把 0 当成空库，必须让调用方去重建。
        assert_eq!(store.browse_file_totals(&library.id, 4).unwrap(), None);
    }

    /// A5-01: Invalid sizes make the projection unusable; valid huge values use TOTAL.
    #[test]
    fn browse_file_totals_rejects_invalid_sizes_and_saturates_safe_totals() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path().join("data")).unwrap();
        let library = store
            .default_library(MediaKind::Movie)
            .unwrap()
            .expect("seeded default movie library");
        let media_id = MediaId::new();
        let scope = format!(
            "{}|{}",
            library.kind.as_str(),
            library
                .root_paths
                .iter()
                .map(|root| root.display().to_string())
                .collect::<Vec<_>>()
                .join("\u{1f}")
        );
        let publish = |generation, sizes: &[serde_json::Value]| {
            let files = sizes
                .iter()
                .enumerate()
                .map(|(index, size)| {
                    let path = format!("/movies/{generation}-{index}.mkv");
                    crate::BrowseFileFacts {
                        ledger_id: LedgerId::new(),
                        media_id,
                        path: path.clone(),
                        facts: serde_json::json!({
                            "version": 3,
                            "path": path,
                            "size": size,
                            "missing": false,
                        }),
                    }
                })
                .collect();
            let build = crate::ProjectionBuild {
                library_id: library.id.clone(),
                generation,
                scope_signature: scope.clone(),
                revisions: store.read_revisions().unwrap(),
                expected_dirty_token: None,
                files,
                media: Vec::new(),
                candidates: Vec::new(),
            };
            assert!(store.publish_browse_projection(&build).unwrap());
        };

        publish(1, &[serde_json::json!(100), serde_json::json!(-90)]);
        assert_eq!(store.browse_file_totals(&library.id, 3).unwrap(), None);
        publish(2, &[serde_json::json!(100), serde_json::json!("invalid")]);
        assert_eq!(store.browse_file_totals(&library.id, 3).unwrap(), None);
        publish(3, &[serde_json::json!(90_000_000_000_000_000u64), serde_json::json!(90_000_000_000_000_000u64)]);
        let totals = store
            .browse_file_totals(&library.id, 3)
            .unwrap()
            .expect("valid bounded sizes aggregate");
        assert_eq!(totals.file_count, 2);
        assert_eq!(totals.total_size_bytes, 180_000_000_000_000_000u64);
        // A pair of values that overflow SQLite SUM(i64) is rejected as invalid
        // facts, not surfaced as an aggregate error/HTTP 500.
        publish(4, &[serde_json::json!(9_000_000_000_000_000_000u64), serde_json::json!(9_000_000_000_000_000_000u64)]);
        assert_eq!(store.browse_file_totals(&library.id, 3).unwrap(), None);
        publish(5, &[serde_json::json!(100_000_000_000_000_001u64)]);
        assert_eq!(
            store.browse_file_totals(&library.id, 3).unwrap(),
            None,
            "sizes above the configured sane bound make facts unusable"
        );
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
