//! 浏览分页读取：SQL 排序键分页、轻候选与本页 facts。
//!
//! 读路径先用可索引的排序键（或轻候选）选出本页 Media，再只解码这些 Media 的
//! `browse_files` 行。无关 Media 的重 facts JSON 不再进入内存；OFFSET 深翻页
//! 仍有 SQLite 自身的跳过成本，本模块不承诺恒定代价。

use std::str::FromStr;

use domain::{MediaId, MediaKind};
use rusqlite::params;

use super::browse_projection::{BrowseFileFacts, BrowseMediaFacts};
use super::browse_reads::MEDIA_BATCH_CHUNK;
use super::maps::parse_id;
use super::{Store, StoreError};

/// 轻候选 JSON 版本。读路径只接受这一版：旧版/缺失即视为投影过旧，调用方回退
/// 到全量读取（与 `facts` JSON 的版本检查同一套语义）。
pub const BROWSE_CANDIDATE_VERSION: u64 = 1;

/// 常用排序的 SQL 索引顺序（无 watch、无过滤时可直接分页）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrowsePageOrder {
    Title,
    AddedAt,
    ReleaseDate,
}

impl BrowsePageOrder {
    /// 排序键列名：`browse_media` 上的索引列。
    fn column(self) -> &'static str {
        match self {
            Self::Title => "title",
            Self::AddedAt => "added_at_ns",
            Self::ReleaseDate => "release_date",
        }
    }
}

/// 一页请求。`limit` 的合法范围由调用方校验。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BrowsePageRequest {
    pub library_id: String,
    pub offset: usize,
    pub limit: usize,
    pub order: BrowsePageOrder,
    pub descending: bool,
}

/// 轻候选：过滤/排序所需的每 Media 标量与小集合。
///
/// **不含** `LedgerRow`，也**不含**整份 files facts JSON。高级查询先用它完整
/// 筛选排序，选页后才定向读取本页重事实。
#[derive(Clone, Debug, PartialEq)]
pub struct BrowseMediaCandidate {
    pub media_id: MediaId,
    pub kind: MediaKind,
    pub title: String,
    pub year: Option<u16>,
    /// 最早入库时间（ns）；`None` 与全量路径的 `None` 同序。
    pub added_at_ns: Option<i64>,
    /// 物化排序键：`metadata.release_date`，缺失时按年份兜底 `YYYY-01-01`，再缺为 `""`。
    pub release_date: String,
    pub rating: Option<f64>,
    pub runtime: Option<u64>,
    pub size_bytes: Option<u64>,
    /// 无任何外部 id（identity 过滤的 provisional）。
    pub provisional: bool,
    pub scraped: bool,
    pub missing: bool,
    /// 任一行 hdr 非空。
    pub hdr: bool,
    /// 去重后的分辨率集合。
    pub resolutions: Vec<String>,
    pub genres: Vec<String>,
    pub countries: Vec<String>,
    pub language: Option<String>,
    pub has_poster: bool,
    pub has_backdrop: bool,
    pub has_still: bool,
    /// 有 season+episode 的行键：观看状态按「存在的分集 unit」判定，与
    /// `watch_state_from_units` 同口径（不能用该 Media 的全部 unit 代替）。
    pub episode_keys: Vec<(u32, u32)>,
}

impl BrowseMediaCandidate {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "version": BROWSE_CANDIDATE_VERSION,
            "media_id": self.media_id.to_string(),
            "kind": self.kind.as_str(),
            "title": self.title,
            "year": self.year,
            "added_at_ns": self.added_at_ns,
            "release_date": self.release_date,
            "rating": self.rating,
            "runtime": self.runtime,
            "size_bytes": self.size_bytes,
            "provisional": self.provisional,
            "scraped": self.scraped,
            "missing": self.missing,
            "hdr": self.hdr,
            "resolutions": self.resolutions,
            "genres": self.genres,
            "countries": self.countries,
            "language": self.language,
            "has_poster": self.has_poster,
            "has_backdrop": self.has_backdrop,
            "has_still": self.has_still,
            "episode_keys": self.episode_keys,
        })
    }

    /// 解析轻候选 JSON；版本不符、字段缺失或类型错误一律返回 `None`。
    pub fn from_json(value: &serde_json::Value) -> Option<Self> {
        if value.get("version")?.as_u64()? != BROWSE_CANDIDATE_VERSION {
            return None;
        }
        let media_id = MediaId::from_str(value.get("media_id")?.as_str()?).ok()?;
        Some(Self {
            media_id,
            kind: value.get("kind")?.as_str()?.parse().ok()?,
            title: value.get("title")?.as_str()?.to_string(),
            year: value.get("year").and_then(serde_json::Value::as_u64).map(|y| y as u16),
            added_at_ns: value.get("added_at_ns").and_then(serde_json::Value::as_i64),
            release_date: value.get("release_date")?.as_str()?.to_string(),
            rating: value.get("rating").and_then(serde_json::Value::as_f64),
            runtime: value.get("runtime").and_then(serde_json::Value::as_u64),
            size_bytes: value.get("size_bytes").and_then(serde_json::Value::as_u64),
            provisional: value.get("provisional")?.as_bool()?,
            scraped: value.get("scraped")?.as_bool()?,
            missing: value.get("missing")?.as_bool()?,
            hdr: value.get("hdr")?.as_bool()?,
            resolutions: string_list(value.get("resolutions")?)?,
            genres: string_list(value.get("genres")?)?,
            countries: string_list(value.get("countries")?)?,
            language: value
                .get("language")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            has_poster: value.get("has_poster")?.as_bool()?,
            has_backdrop: value.get("has_backdrop")?.as_bool()?,
            has_still: value.get("has_still")?.as_bool()?,
            episode_keys: pair_list(value.get("episode_keys")?)?,
        })
    }
}

fn string_list(value: &serde_json::Value) -> Option<Vec<String>> {
    value
        .as_array()?
        .iter()
        .map(|item| item.as_str().map(str::to_string))
        .collect()
}

fn pair_list(value: &serde_json::Value) -> Option<Vec<(u32, u32)>> {
    value
        .as_array()?
        .iter()
        .map(|item| {
            let pair = item.as_array()?;
            Some((pair.first()?.as_u64()? as u32, pair.get(1)?.as_u64()? as u32))
        })
        .collect()
}

impl Store {
    /// 常用排序的一页 `browse_media` 行（含 facts JSON）。
    ///
    /// 只按索引列排序取 `LIMIT/OFFSET` 行，与全量路径的 `compare` 逐字同序：
    /// 排序键相同的行按 `media_id` 升序（`compare` 的稳定并列键），降序只反转
    /// 主键（并列仍是升序）。没有 file 行的 Media 不出现（全量路径同样跳过它们）。
    pub fn browse_media_page(
        &self,
        request: &BrowsePageRequest,
    ) -> Result<Vec<BrowseMediaFacts>, StoreError> {
        self.browse_media_page_with_vm_steps(request).map(|(rows, _)| rows)
    }

    /// Same read as [`Self::browse_media_page`], additionally reporting the SQLite VM
    /// steps so tests can assert DB work instead of only returned rows.
    #[doc(hidden)]
    pub fn browse_media_page_with_vm_steps(
        &self,
        request: &BrowsePageRequest,
    ) -> Result<(Vec<BrowseMediaFacts>, i64), StoreError> {
        let direction = if request.descending { "DESC" } else { "ASC" };
        let column = request.order.column();
        let sql = format!(
            "SELECT m.media_id, m.title, m.year, m.facts_json
             FROM browse_media m
             WHERE m.library_id = ?1
               AND EXISTS (SELECT 1 FROM browse_files f
                           WHERE f.library_id = m.library_id AND f.media_id = m.media_id)
             ORDER BY m.{column} {direction}, m.media_id ASC
             LIMIT ?2 OFFSET ?3"
        );
        let mut stmt = self.library.prepare(&sql)?;
        // `usize` 极值必须饱和到 i64：直接 `as i64` 会把 `usize::MAX` 变成 -1，
        // SQLite 视负 OFFSET 为 0，越界翻页会错误地回到第一页。
        let limit = i64::try_from(request.limit).unwrap_or(i64::MAX);
        let offset = i64::try_from(request.offset).unwrap_or(i64::MAX);
        let rows = stmt.query_map(
            params![request.library_id, limit, offset],
            map_media_facts,
        )?;
        let collected = rows.collect::<Result<Vec<_>, _>>()?;
        let vm_steps = i64::from(stmt.get_status(rusqlite::StatementStatus::VmStep));
        Ok((collected, vm_steps))
    }

    /// 指定 Media 的投影行（含 facts JSON）：轻候选选页后水合本页用。
    pub fn browse_media_facts_for(
        &self,
        library_id: &str,
        ids: &[MediaId],
    ) -> Result<Vec<BrowseMediaFacts>, StoreError> {
        let mut found = Vec::with_capacity(ids.len());
        for chunk in ids.chunks(MEDIA_BATCH_CHUNK) {
            let placeholders = vec!["?"; chunk.len()].join(", ");
            let sql = format!(
                "SELECT media_id, title, year, facts_json
                 FROM browse_media
                 WHERE library_id = ?1 AND media_id IN ({placeholders})"
            );
            let mut values: Vec<rusqlite::types::Value> =
                vec![rusqlite::types::Value::from(library_id.to_string())];
            values.extend(
                chunk
                    .iter()
                    .map(|id| rusqlite::types::Value::from(id.to_string())),
            );
            let mut stmt = self.library.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(values), map_media_facts)?;
            for row in rows {
                found.push(row?);
            }
        }
        Ok(found)
    }

    /// 选中 Media 的 file facts：只解码这些 Media 的 `browse_files` 行。
    pub fn browse_file_facts_for_media(
        &self,
        library_id: &str,
        ids: &[MediaId],
    ) -> Result<Vec<BrowseFileFacts>, StoreError> {
        let mut found = Vec::new();
        for chunk in ids.chunks(MEDIA_BATCH_CHUNK) {
            let placeholders = vec!["?"; chunk.len()].join(", ");
            let sql = format!(
                "SELECT ledger_id, media_id, facts_json
                 FROM browse_files
                 WHERE library_id = ?1 AND media_id IN ({placeholders})"
            );
            let mut values: Vec<rusqlite::types::Value> =
                vec![rusqlite::types::Value::from(library_id.to_string())];
            values.extend(
                chunk
                    .iter()
                    .map(|id| rusqlite::types::Value::from(id.to_string())),
            );
            let mut stmt = self.library.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(values), |row| {
                let ledger_id = parse_id(row.get::<_, String>(0)?, 0)?;
                let media_id = parse_id(row.get::<_, String>(1)?, 1)?;
                let facts_str: String = row.get(2)?;
                Ok(BrowseFileFacts {
                    ledger_id,
                    media_id,
                    facts: serde_json::from_str(&facts_str).unwrap_or_default(),
                })
            })?;
            for row in rows {
                found.push(row?);
            }
        }
        Ok(found)
    }

    /// 整库轻候选：不读 facts JSON，只读每 Media 的标量与小集合。
    ///
    /// 高级查询（过滤/watch/非常用排序）用它完整筛选排序；成本 O(M) 小行，
    /// 与「解码整库重 facts」是两件事。
    pub fn browse_media_candidates(
        &self,
        library_id: &str,
    ) -> Result<Vec<BrowseMediaCandidate>, StoreError> {
        let mut stmt = self.library.prepare(
            "SELECT candidate_json FROM browse_media m
             WHERE m.library_id = ?1
               AND EXISTS (SELECT 1 FROM browse_files f
                           WHERE f.library_id = m.library_id AND f.media_id = m.media_id)",
        )?;
        let rows = stmt.query_map(params![library_id], |row| row.get::<_, String>(0))?;
        let mut candidates = Vec::new();
        for row in rows {
            let text = row?;
            let value: serde_json::Value = serde_json::from_str(&text)?;
            let candidate = BrowseMediaCandidate::from_json(&value).ok_or_else(|| {
                StoreError::Invalid(format!(
                    "browse media candidate version or field mismatch: {library_id}"
                ))
            })?;
            candidates.push(candidate);
        }
        Ok(candidates)
    }
}

fn map_media_facts(row: &rusqlite::Row<'_>) -> rusqlite::Result<BrowseMediaFacts> {
    let media_id = parse_id(row.get::<_, String>(0)?, 0)?;
    let title: String = row.get(1)?;
    let year: Option<u16> = row.get(2)?;
    let facts_str: String = row.get(3)?;
    Ok(BrowseMediaFacts {
        media_id,
        title,
        year,
        facts: serde_json::from_str(&facts_str).unwrap_or_default(),
    })
}
