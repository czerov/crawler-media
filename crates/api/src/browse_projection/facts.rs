//! 浏览投影里保存的**已校验事实**。
//!
//! 构建器（写）与浏览读路径（读）共用同一套结构：读路径不得再做 NFO/stat/
//! `read_dir`，因此所有展示字段必须在构建时落到这里。JSON 里带 `version`，
//! 旧格式或解析失败的投影一律视为不可用，调用方回退到旧的全量读取。

use domain::{Confidence, LedgerId, LedgerRow, Media, MediaId, MediaKind, QualitySource};
use serde_json::{Value, json};
use std::str::FromStr;

use crate::http::library::selection::metadata::BrowseMetadata;

/// 当前 facts JSON 版本。读路径只接受这一版。
pub(crate) const FACTS_VERSION: u64 = 3;

/// 单个 ledger 文件的投影事实。
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FileFacts {
    pub row: LedgerRow,
    /// `metadata()` 成功且是普通文件时的大小；否则 0。
    pub size: u64,
    /// 是否成功观察到文件元数据（决定 size/added_at 是否计入）。
    pub has_stat: bool,
    pub created_ns: Option<i64>,
    pub modified_ns: Option<i64>,
    pub missing: bool,
    pub has_still: bool,
    pub sidecars: serde_json::Map<String, Value>,
}

impl FileFacts {
    /// 与 `file_stats::aggregate_added_at` 同一口径：created 优先、modified 兜底。
    pub fn added_at_ns(&self) -> Option<i64> {
        self.created_ns.or(self.modified_ns)
    }

    pub fn to_json(&self) -> Value {
        let row = &self.row;
        json!({
            "version": FACTS_VERSION,
            "ledger_id": row.id.to_string(),
            "media_id": row.media_id.to_string(),
            "path": row.path,
            "season": row.season,
            "episode": row.episode,
            "resolution": row.resolution,
            "codec": row.codec,
            "hdr": row.hdr,
            "quality_source": row.quality_source.as_str(),
            "confidence": row.confidence.as_str(),
            "filter_score": row.filter_score,
            "size": self.size,
            "has_stat": self.has_stat,
            "created_ns": self.created_ns,
            "modified_ns": self.modified_ns,
            "missing": self.missing,
            "has_still": self.has_still,
            "sidecars": self.sidecars,
        })
    }

    pub fn from_json(value: &Value) -> Option<Self> {
        if value.get("version")?.as_u64()? != FACTS_VERSION {
            return None;
        }
        let row = LedgerRow {
            id: LedgerId::from_str(value.get("ledger_id")?.as_str()?).ok()?,
            media_id: MediaId::from_str(value.get("media_id")?.as_str()?).ok()?,
            path: value.get("path")?.as_str()?.to_string(),
            season: opt_u32(value, "season")?,
            episode: opt_u32(value, "episode")?,
            resolution: opt_string(value, "resolution")?,
            codec: opt_string(value, "codec")?,
            hdr: opt_string(value, "hdr")?,
            quality_source: parse_quality_source(value.get("quality_source")?.as_str()?)?,
            confidence: parse_confidence(value.get("confidence")?.as_str()?)?,
            filter_score: value
                .get("filter_score")
                .and_then(Value::as_i64)
                .map(|v| v as i32),
        };
        Some(Self {
            row,
            size: value.get("size")?.as_u64()?,
            has_stat: value.get("has_stat")?.as_bool()?,
            created_ns: value.get("created_ns").and_then(Value::as_i64),
            modified_ns: value.get("modified_ns").and_then(Value::as_i64),
            missing: value.get("missing")?.as_bool()?,
            has_still: value.get("has_still")?.as_bool()?,
            sidecars: value.get("sidecars")?.as_object()?.clone(),
        })
    }
}

/// 单个 Media 的投影事实。
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MediaFacts {
    pub media: Media,
    pub metadata: BrowseMetadata,
    /// 首选文件旁（或其上一层）存在 poster.jpg。
    pub has_poster: bool,
    /// 首选文件旁（或其上一层）存在 fanart.jpg。
    pub has_backdrop: bool,
}

impl MediaFacts {
    pub fn to_json(&self) -> Value {
        let media = &self.media;
        let meta = &self.metadata;
        json!({
            "version": FACTS_VERSION,
            "media": {
                "id": media.id.to_string(),
                "kind": media.kind.as_str(),
                "title": media.title,
                "year": media.year,
                "original_title": media.original_title,
                "tmdb_id": media.tmdb_id,
                "douban_id": media.douban_id,
                "tvdb_id": media.tvdb_id,
                "bangumi_id": media.bangumi_id,
                "anilist_id": media.anilist_id,
            },
            "metadata": {
                "genres": &meta.genres,
                "countries": &meta.countries,
                "rating": meta.rating,
                "runtime": meta.runtime,
                "language": &meta.language,
                "release_date": &meta.release_date,
                "scraped": meta.scraped,
            },
            "has_poster": self.has_poster,
            "has_backdrop": self.has_backdrop,
        })
    }

    pub fn from_json(value: &Value) -> Option<Self> {
        if value.get("version")?.as_u64()? != FACTS_VERSION {
            return None;
        }
        let media_json = value.get("media")?;
        let meta_json = value.get("metadata")?;
        let media = Media {
            id: MediaId::from_str(media_json.get("id")?.as_str()?).ok()?,
            kind: MediaKind::from_str(media_json.get("kind")?.as_str()?).ok()?,
            title: media_json.get("title")?.as_str()?.to_string(),
            year: media_json
                .get("year")
                .and_then(Value::as_u64)
                .map(|v| v as u16),
            original_title: opt_string(media_json, "original_title")?,
            tmdb_id: opt_string(media_json, "tmdb_id")?,
            douban_id: opt_string(media_json, "douban_id")?,
            tvdb_id: opt_string(media_json, "tvdb_id")?,
            bangumi_id: opt_string(media_json, "bangumi_id")?,
            anilist_id: opt_string(media_json, "anilist_id")?,
        };
        let metadata = BrowseMetadata {
            genres: string_list(meta_json, "genres")?,
            countries: string_list(meta_json, "countries")?,
            rating: meta_json.get("rating").and_then(Value::as_f64),
            runtime: meta_json.get("runtime").and_then(Value::as_u64),
            language: opt_string(meta_json, "language")?,
            release_date: opt_string(meta_json, "release_date")?,
            scraped: meta_json.get("scraped")?.as_bool()?,
        };
        Some(Self {
            media,
            metadata,
            has_poster: value.get("has_poster")?.as_bool()?,
            has_backdrop: value.get("has_backdrop")?.as_bool()?,
        })
    }
}

fn opt_string(value: &Value, key: &str) -> Option<Option<String>> {
    match value.get(key) {
        None | Some(Value::Null) => Some(None),
        Some(Value::String(text)) => Some(Some(text.clone())),
        Some(_) => None,
    }
}

fn opt_u32(value: &Value, key: &str) -> Option<Option<u32>> {
    match value.get(key) {
        None | Some(Value::Null) => Some(None),
        Some(found) => found.as_u64().map(|v| Some(v as u32)),
    }
}

fn string_list(value: &Value, key: &str) -> Option<Vec<String>> {
    let array = value.get(key)?.as_array()?;
    let mut out = Vec::with_capacity(array.len());
    for item in array {
        out.push(item.as_str()?.to_string());
    }
    Some(out)
}

fn parse_quality_source(text: &str) -> Option<QualitySource> {
    match text {
        "probe" => Some(QualitySource::Probe),
        "release" => Some(QualitySource::Release),
        _ => None,
    }
}

fn parse_confidence(text: &str) -> Option<Confidence> {
    match text {
        "high" => Some(Confidence::High),
        "low" => Some(Confidence::Low),
        _ => None,
    }
}
