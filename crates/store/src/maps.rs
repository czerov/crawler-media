use std::str::FromStr;

use domain::{
    Confidence, Coverage, FilterAtom, LedgerRow, Media, QualitySource, Site, SiteId, Subscribe,
    Torrent,
};

use super::StoreError;

pub(super) fn parse_id<T: FromStr>(raw: String, idx: usize) -> rusqlite::Result<T>
where
    T::Err: std::error::Error + Send + Sync + 'static,
{
    T::from_str(&raw).map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(idx, rusqlite::types::Type::Text, Box::new(err))
    })
}

pub(super) fn parse_token<T: FromStr>(raw: String, idx: usize) -> rusqlite::Result<T>
where
    T::Err: std::fmt::Display,
{
    T::from_str(&raw).map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(
            idx,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                err.to_string(),
            )),
        )
    })
}

fn aliases_from_json(raw: Option<String>) -> Vec<String> {
    let Some(raw) = raw.filter(|raw| !raw.is_empty()) else {
        return Vec::new();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

pub(super) fn aliases_json(aliases: &[String]) -> String {
    serde_json::to_string(aliases).unwrap_or_else(|_| "[]".to_string())
}

pub(super) fn map_media(row: &rusqlite::Row<'_>) -> rusqlite::Result<Media> {
    let kind: String = row.get(1)?;
    Ok(Media {
        id: parse_id(row.get(0)?, 0)?,
        kind: parse_token(kind, 1)?,
        title: row.get(2)?,
        year: row.get::<_, Option<i64>>(3)?.map(|v| v as u16),
        original_title: row.get(4)?,
        aliases: aliases_from_json(row.get(10)?),
        tmdb_id: row.get(5)?,
        douban_id: row.get(6)?,
        tvdb_id: row.get(7)?,
        bangumi_id: row.get(8)?,
        anilist_id: row.get(9)?,
    })
}

pub(super) fn map_ledger(row: &rusqlite::Row<'_>) -> rusqlite::Result<LedgerRow> {
    let quality_source: String = row.get(8)?;
    let confidence: String = row.get(9)?;
    Ok(LedgerRow {
        id: parse_id(row.get(0)?, 0)?,
        media_id: parse_id(row.get(1)?, 1)?,
        path: row.get(2)?,
        season: row.get::<_, Option<i64>>(3)?.map(|v| v as u32),
        episode: row.get::<_, Option<i64>>(4)?.map(|v| v as u32),
        resolution: row.get(5)?,
        codec: row.get(6)?,
        hdr: row.get(7)?,
        quality_source: match quality_source.as_str() {
            "probe" => QualitySource::Probe,
            _ => QualitySource::Release,
        },
        confidence: match confidence.as_str() {
            "low" => Confidence::Low,
            _ => Confidence::High,
        },
        filter_score: row.get(10)?,
    })
}

pub(super) fn map_subscribe(row: &rusqlite::Row<'_>) -> rusqlite::Result<Subscribe> {
    let coverage_kind: String = row.get(3)?;
    let coverage = if coverage_kind == "tv" {
        Coverage::Tv {
            season: row.get::<_, i64>(4)? as u32,
            episode_from: row.get::<_, i64>(5)? as u32,
            episode_to: row.get::<_, Option<i64>>(6)?.map(|v| v as u32),
        }
    } else {
        Coverage::Movie
    };
    let fetch_mode: String = row.get(7)?;
    Ok(Subscribe {
        id: parse_id(row.get(0)?, 0)?,
        user_id: parse_id(row.get(1)?, 1)?,
        media_id: parse_id(row.get(2)?, 2)?,
        coverage,
        fetch_mode: parse_token(fetch_mode, 7)?,
        filter_id: parse_id(row.get(8)?, 8)?,
        wash_cut: row.get::<_, i64>(9)? != 0,
        wash_cut_filter_id: row
            .get::<_, Option<String>>(10)?
            .map(|s| parse_id(s, 10))
            .transpose()?,
        full_season_pack: row.get::<_, i64>(11)? != 0,
        downloader_id: row
            .get::<_, Option<String>>(12)?
            .map(|s| parse_id(s, 12))
            .transpose()?,
        tracking_state: row.get::<_, String>(13)?,
        follow_future: row.get::<_, i64>(14)? != 0,
        search_interval_secs: row.get::<_, i64>(15)? as u32,
        keep_old_versions: row.get::<_, i64>(16)? != 0,
        library_id: row
            .get::<_, Option<String>>(17)?
            .map(|s| parse_id(s, 17))
            .transpose()?,
    })
}

pub(super) fn map_site(row: &rusqlite::Row<'_>) -> rusqlite::Result<Site> {
    Ok(Site {
        id: parse_id(row.get(0)?, 0)?,
        name: row.get(1)?,
        url: row.get(2)?,
        profile_id: row.get(3)?,
        cookie: row.get(4)?,
        api_key: row.get(5)?,
        rss_url: row.get(6)?,
        proxy: row.get(7)?,
        rate_limit_per_minute: row.get(8)?,
        cdp_url: row.get(9)?,
        downloader_id: row
            .get::<_, Option<String>>(10)?
            .map(|s| parse_id(s, 10))
            .transpose()?,
        enabled: row.get::<_, i64>(11)? != 0,
        maintenance_via: row
            .get::<_, Option<String>>(12)?
            .as_deref()
            .and_then(|s| s.parse().ok())
            .unwrap_or_default(),
    })
}

/// Serialize filter atoms as a JSON array.
///
/// This replaced the legacy compact DSL (`100:resolution=2160p|50:source=bluray`).
/// `parse_atoms` still reads the old form so existing rows keep working; every
/// write after this migration produces JSON.
pub(super) fn serialize_atoms(atoms: &[FilterAtom]) -> String {
    let values: Vec<serde_json::Value> = atoms
        .iter()
        .map(|atom| {
            let (kind, value) = match &atom.rule {
                domain::AtomRule::Resolution(v) => ("resolution", Some(serde_json::json!(v))),
                domain::AtomRule::Source(v) => ("source", Some(serde_json::json!(v))),
                domain::AtomRule::Codec(v) => ("video_codec", Some(serde_json::json!(v))),
                domain::AtomRule::Free => ("free", None),
                domain::AtomRule::Hr => ("hr", None),
                domain::AtomRule::TitleMatch(v) => ("title", Some(serde_json::json!(v))),
                domain::AtomRule::Hdr(v) => ("hdr", Some(serde_json::json!(v))),
                domain::AtomRule::Size { min_mb, max_mb } => (
                    "size",
                    Some(serde_json::json!({
                        "min_mb": min_mb,
                        "max_mb": max_mb,
                    })),
                ),
                domain::AtomRule::MinSeeders(n) => ("min_seeders", Some(serde_json::json!(n))),
                domain::AtomRule::SubtitleLanguage(v) => ("subtitle", Some(serde_json::json!(v))),
                domain::AtomRule::AudioLanguage(v) => ("audio", Some(serde_json::json!(v))),
                domain::AtomRule::Site(v) => ("site", Some(serde_json::json!(v))),
                domain::AtomRule::WashTarget(v) => ("wash_target", Some(serde_json::json!(v))),
                domain::AtomRule::UpgradeLadder(v) => {
                    ("upgrade_ladder", Some(serde_json::json!(v)))
                }
            };
            let mut obj = serde_json::Map::new();
            obj.insert("priority".into(), serde_json::json!(atom.priority));
            obj.insert("rule".into(), serde_json::json!(kind));
            obj.insert("exclude".into(), serde_json::json!(atom.exclude));
            if let Some(value) = value {
                obj.insert("value".into(), value);
            }
            serde_json::Value::Object(obj)
        })
        .collect();
    serde_json::Value::Array(values).to_string()
}

/// Parse filter atoms from storage.
///
/// Accepts both the JSON form written since this migration and the legacy
/// compact DSL, so a database written by an older build still loads.
pub(super) fn parse_atoms(raw: &str) -> Vec<FilterAtom> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    if trimmed.starts_with('[') {
        return parse_atoms_json(trimmed);
    }
    parse_atoms_dsl(trimmed)
}

/// Text value of a JSON atom payload; accepts a string or a number.
fn atom_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        _ => String::new(),
    }
}

fn parse_atoms_json(raw: &str) -> Vec<FilterAtom> {
    let Ok(serde_json::Value::Array(items)) = serde_json::from_str::<serde_json::Value>(raw) else {
        return Vec::new();
    };
    items
        .into_iter()
        .filter_map(|item| {
            let priority = item["priority"].as_i64()? as i32;
            let kind = item["rule"].as_str()?;
            let exclude = item["exclude"].as_bool().unwrap_or(false);
            let value = item.get("value").unwrap_or(&serde_json::Value::Null);
            let text = atom_text(value);
            let rule = match kind {
                "resolution" => domain::AtomRule::Resolution(text),
                "source" => domain::AtomRule::Source(text),
                "video_codec" => domain::AtomRule::Codec(text),
                "free" => domain::AtomRule::Free,
                "hr" => domain::AtomRule::Hr,
                "title" => domain::AtomRule::TitleMatch(text),
                "hdr" => domain::AtomRule::Hdr(text),
                "size" => domain::AtomRule::Size {
                    min_mb: value["min_mb"].as_u64().filter(|m| *m > 0),
                    max_mb: value["max_mb"].as_u64().filter(|m| *m > 0),
                },
                "min_seeders" => domain::AtomRule::MinSeeders(text.parse().ok()?),
                "subtitle" => domain::AtomRule::SubtitleLanguage(text),
                "audio" => domain::AtomRule::AudioLanguage(text),
                "site" => domain::AtomRule::Site(text),
                "wash_target" => domain::AtomRule::WashTarget(text),
                "upgrade_ladder" => domain::AtomRule::UpgradeLadder(text),
                _ => return None,
            };
            Some(FilterAtom {
                priority,
                rule,
                exclude,
            })
        })
        .collect()
}

/// Legacy compact DSL: `100:resolution=2160p|50:!source=cam`.
fn parse_atoms_dsl(raw: &str) -> Vec<FilterAtom> {
    raw.split('|')
        .filter_map(|part| {
            let (priority, rule) = part.split_once(':')?;
            let priority = priority.parse().ok()?;
            let (exclude, rule) = match rule.strip_prefix('!') {
                Some(rest) => (true, rest),
                None => (false, rule),
            };
            let rule = if let Some(v) = rule.strip_prefix("resolution=") {
                domain::AtomRule::Resolution(v.to_string())
            } else if let Some(v) = rule.strip_prefix("source=") {
                domain::AtomRule::Source(v.to_string())
            } else if rule == "free" {
                domain::AtomRule::Free
            } else if rule == "hr" {
                domain::AtomRule::Hr
            } else if let Some(v) = rule.strip_prefix("title=") {
                domain::AtomRule::TitleMatch(v.to_string())
            } else if let Some(v) = rule.strip_prefix("hdr=") {
                domain::AtomRule::Hdr(v.to_string())
            } else if let Some(v) = rule.strip_prefix("size=") {
                let (min, max) = v.split_once('-').unwrap_or((v, ""));
                domain::AtomRule::Size {
                    min_mb: min.parse().ok().filter(|m| *m > 0),
                    max_mb: max.parse().ok().filter(|m| *m > 0),
                }
            } else if let Some(v) = rule.strip_prefix("min_seeders=") {
                domain::AtomRule::MinSeeders(v.parse().ok()?)
            } else if let Some(v) = rule.strip_prefix("subtitle=") {
                domain::AtomRule::SubtitleLanguage(v.to_string())
            } else if let Some(v) = rule.strip_prefix("audio=") {
                domain::AtomRule::AudioLanguage(v.to_string())
            } else if let Some(v) = rule.strip_prefix("site=") {
                domain::AtomRule::Site(v.to_string())
            } else if let Some(v) = rule.strip_prefix("wash_target=") {
                domain::AtomRule::WashTarget(v.to_string())
            } else if let Some(v) = rule.strip_prefix("upgrade_ladder=") {
                domain::AtomRule::UpgradeLadder(v.to_string())
            } else {
                return None;
            };
            Some(FilterAtom {
                priority,
                rule,
                exclude,
            })
        })
        .collect()
}

pub(super) fn torrent_from_json(value: &serde_json::Value) -> Result<Torrent, StoreError> {
    let site_id = value
        .get("site_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| StoreError::FetchMode("pending torrent site_id".into()))?;
    Ok(Torrent {
        site_id: SiteId::from_str(site_id)?,
        title: value
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        enclosure: value
            .get("enclosure")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        size_bytes: value.get("size_bytes").and_then(|v| v.as_u64()),
        seeders: value
            .get("seeders")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32),
        free: value.get("free").and_then(|v| v.as_bool()).unwrap_or(false),
        hr: value.get("hr").and_then(|v| v.as_bool()).unwrap_or(false),
        imdb_id: value
            .get("imdb_id")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        id: value.get("id").and_then(|v| v.as_str()).map(str::to_string),
        leechers: value
            .get("leechers")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32),
        snatched: value
            .get("snatched")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32),
        upload_time: value
            .get("upload_time")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        detail_url: value
            .get("detail_url")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        category: value
            .get("category")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        poster_url: value
            .get("poster_url")
            .and_then(|v| v.as_str())
            .map(str::to_string),
    })
}
