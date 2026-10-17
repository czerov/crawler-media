//! 从目标 Library 的 ready 投影重建请求级浏览快照。
//!
//! 这里**不做任何文件 IO**：NFO/stat/海报/剧照事实都在构建投影时已经落库。
//! 任一 facts JSON 不可解析即整体判为不可用，调用方回退到旧的全量读取。

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use domain::{LedgerId, LedgerRow, MediaId, UserId};
use store::{ProjectionSnapshot, Store};

use crate::browse_projection::facts::{FileFacts, MediaFacts};
use crate::http::library::{backdrop_path_with, poster_path_with, preferred_row};

use super::snapshot::{BrowseGroup, BrowseSnapshot, ProjectedGroup};

/// 一个 Media 的图片存在性（墙面与图集共用）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ArtworkFacts {
    pub has_poster: bool,
    pub has_backdrop: bool,
}

/// 投影携带的图片/剧照事实，供渲染时零 IO 查询。
pub(crate) struct ProjectedFacts {
    pub artwork: HashMap<MediaId, ArtworkFacts>,
    pub stills: HashSet<LedgerId>,
}

/// 图片事实来源：投影命中即零 IO，否则按旧语义现场探测（回退路径）。
pub(crate) struct BrowseArtwork<'a> {
    projected: Option<&'a ProjectedFacts>,
    files: &'a dyn crate::browse_file_read::BrowseFileRead,
}

impl<'a> BrowseArtwork<'a> {
    pub(crate) fn new(
        projected: Option<&'a ProjectedFacts>,
        files: &'a dyn crate::browse_file_read::BrowseFileRead,
    ) -> Self {
        Self { projected, files }
    }

    pub(crate) fn media(&self, media_id: MediaId, rows: &[LedgerRow]) -> ArtworkFacts {
        if let Some(projected) = self.projected {
            return projected.artwork.get(&media_id).copied().unwrap_or_default();
        }
        let Some(row) = preferred_row(rows) else {
            return ArtworkFacts::default();
        };
        ArtworkFacts {
            has_poster: poster_path_with(row, self.files).is_some(),
            has_backdrop: backdrop_path_with(row, self.files).is_some(),
        }
    }

    pub(crate) fn has_still(&self, row: &LedgerRow) -> bool {
        if let Some(projected) = self.projected {
            return projected.stills.contains(&row.id);
        }
        crate::episode_still::existing_with(Path::new(&row.path), self.files).is_some()
    }
}

/// 投影 → [`BrowseSnapshot`]：逐 Media 组装展示事实，用户 unit 仍按目标 Media 批量读取。
pub(crate) fn snapshot_from_projection(
    store: &Store,
    snapshot: &ProjectionSnapshot,
    user: Option<UserId>,
) -> Result<(BrowseSnapshot, ProjectedFacts), String> {
    let mut media_facts: HashMap<MediaId, MediaFacts> = HashMap::new();
    let mut order: Vec<MediaId> = Vec::with_capacity(snapshot.media.len());
    for entry in &snapshot.media {
        let facts = MediaFacts::from_json(&entry.facts)
            .ok_or_else(|| format!("media facts unreadable for {}", entry.media_id))?;
        order.push(entry.media_id);
        media_facts.insert(entry.media_id, facts);
    }

    let mut rows_by_media: HashMap<MediaId, Vec<LedgerRow>> = HashMap::new();
    let mut facts_by_media: HashMap<MediaId, Vec<FileFacts>> = HashMap::new();
    let mut stills = HashSet::new();
    for entry in &snapshot.files {
        let facts = FileFacts::from_json(&entry.facts)
            .ok_or_else(|| format!("file facts unreadable for {}", entry.ledger_id))?;
        if facts.has_still {
            stills.insert(facts.row.id);
        }
        rows_by_media
            .entry(facts.row.media_id)
            .or_default()
            .push(facts.row.clone());
        facts_by_media
            .entry(facts.row.media_id)
            .or_default()
            .push(facts);
    }

    let mut groups = Vec::with_capacity(order.len());
    let mut artwork = HashMap::new();
    for media_id in order {
        let Some(media) = media_facts.remove(&media_id) else {
            continue;
        };
        let rows = rows_by_media.remove(&media_id).unwrap_or_default();
        if rows.is_empty() {
            continue;
        }
        let facts = facts_by_media.remove(&media_id).unwrap_or_default();
        artwork.insert(
            media_id,
            ArtworkFacts {
                has_poster: media.has_poster,
                has_backdrop: media.has_backdrop,
            },
        );
        let projected = ProjectedGroup {
            metadata: media.metadata,
            missing: facts.iter().any(|fact| fact.missing),
            added_at: min_added_at(&facts)?,
            size_bytes: total_size(&facts),
        };
        groups.push(BrowseGroup {
            media: media.media,
            rows,
            projected: Some(projected),
        });
    }

    let ids: Vec<MediaId> = groups.iter().map(|group| group.media.id).collect();
    let units = match user {
        Some(user) => store
            .playback_units_for_media(user, &ids)
            .map_err(|error| error.to_string())?
            .into_iter()
            .map(|unit| ((unit.media_id, unit.season, unit.episode), unit.state))
            .collect(),
        None => HashMap::new(),
    };
    Ok((
        BrowseSnapshot {
            library_id: snapshot.library_id.clone(),
            groups,
            cache: Vec::new(),
            missing: HashMap::new(),
            units,
            user,
        },
        ProjectedFacts { artwork, stills },
    ))
}

/// 与 `file_stats::aggregate_added_at` 同口径：created 优先、modified 兜底，取最早。
pub(crate) fn min_added_at(facts: &[FileFacts]) -> Result<Option<SystemTime>, String> {
    facts
        .iter()
        .filter_map(FileFacts::added_at_ns)
        .min()
        .map(ns_to_system_time)
        .transpose()
}

/// 与 `file_stats::aggregate_size` 同口径：只有观察到普通文件的行计入。
pub(crate) fn total_size(facts: &[FileFacts]) -> Option<u64> {
    let mut total = 0u64;
    let mut known = false;
    for fact in facts {
        if fact.has_stat {
            total += fact.size;
            known = true;
        }
    }
    known.then_some(total)
}

pub(crate) fn ns_to_system_time(ns: i64) -> Result<SystemTime, String> {
    let shifted = if ns >= 0 {
        UNIX_EPOCH.checked_add(Duration::from_nanos(ns as u64))
    } else {
        UNIX_EPOCH.checked_sub(Duration::from_nanos(ns.unsigned_abs()))
    };
    shifted.ok_or_else(|| format!("file timestamp out of range: {ns}"))
}
