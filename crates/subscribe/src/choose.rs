use domain::{Coverage, Media, MediaKind, Release, Subscribe};
use filter::ScoredTorrent;

use crate::facts::SubscribeFacts;

pub fn choose(
    subscribe: &Subscribe,
    wash_filter: Option<&domain::Filter>,
    admitted: &[ScoredTorrent],
    facts: &SubscribeFacts,
) -> Vec<ScoredTorrent> {
    choose_with_policy(
        subscribe,
        wash_filter,
        admitted,
        facts,
        OverlapPolicy::default(),
    )
}

/// How overlapping TV candidates (range pack vs single episodes) are resolved (M1).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OverlapPolicy {
    /// Larger packs claim their slots first.
    #[default]
    PreferPack,
    /// Single episodes claim their slots first; packs only fill untouched slots.
    PreferEpisode,
}

impl OverlapPolicy {
    pub fn parse(value: &str) -> Self {
        match value.trim() {
            "prefer_episode" => Self::PreferEpisode,
            _ => Self::PreferPack,
        }
    }
}

/// M1 greedy coverage: candidates are ordered by policy (slot count) then
/// score, and one is chosen only when none of its window slots is already
/// claimed, so overlapping packs and singles are never downloaded together.
fn choose_tv_greedy(
    subscribe: &Subscribe,
    wash_filter: Option<&domain::Filter>,
    admitted: &[ScoredTorrent],
    facts: &SubscribeFacts,
    policy: OverlapPolicy,
) -> Vec<ScoredTorrent> {
    let mut ranked: Vec<(&ScoredTorrent, Vec<(u32, u32)>)> = admitted
        .iter()
        .filter(|c| should_take(subscribe, wash_filter, facts, c))
        .map(|c| {
            // 只竞争订阅窗口里这个候选真正能填的槽。
            let slots = covered_slots(subscribe, &c.release)
                .into_iter()
                .filter_map(|(s, e)| Some((s?, e?)))
                .collect::<Vec<_>>();
            (c, slots)
        })
        .filter(|(_, slots)| !slots.is_empty())
        .collect();
    ranked.sort_by(|(a, a_slots), (b, b_slots)| {
        let by_size = match policy {
            OverlapPolicy::PreferPack => b_slots.len().cmp(&a_slots.len()),
            OverlapPolicy::PreferEpisode => a_slots.len().cmp(&b_slots.len()),
        };
        by_size.then(b.score.cmp(&a.score))
    });
    let mut claimed: std::collections::HashSet<(u32, u32)> = std::collections::HashSet::new();
    let mut chosen: Vec<ScoredTorrent> = Vec::new();
    for (candidate, slots) in ranked {
        if slots.iter().any(|slot| claimed.contains(slot))
            || chosen
                .iter()
                .any(|c| c.torrent.enclosure == candidate.torrent.enclosure)
        {
            continue;
        }
        claimed.extend(slots);
        chosen.push(candidate.clone());
    }
    chosen
}

pub fn choose_with_policy(
    subscribe: &Subscribe,
    wash_filter: Option<&domain::Filter>,
    admitted: &[ScoredTorrent],
    facts: &SubscribeFacts,
    policy: OverlapPolicy,
) -> Vec<ScoredTorrent> {
    let mut chosen = Vec::new();
    if subscribe.full_season_pack {
        let average = single_episode_average_size(admitted);
        if let Some(pack) = admitted
            .iter()
            .filter(|c| is_pack(&c.release, subscribe))
            .filter(|c| pack_size_plausible(c, subscribe, average))
            .filter(|c| should_take(subscribe, wash_filter, facts, c))
            .max_by_key(|c| c.score)
        {
            chosen.push(pack.clone());
        }
        return chosen;
    }

    match subscribe.coverage {
        Coverage::Movie => {
            if let Some(best) = admitted
                .iter()
                .filter(|c| should_take(subscribe, wash_filter, facts, c))
                .max_by_key(|c| c.score)
            {
                chosen.push(best.clone());
            }
        }
        Coverage::Tv { .. } => {
            chosen = choose_tv_greedy(subscribe, wash_filter, admitted, facts, policy);
        }
    }
    chosen
}

fn should_take(
    subscribe: &Subscribe,
    wash_filter: Option<&domain::Filter>,
    facts: &SubscribeFacts,
    candidate: &ScoredTorrent,
) -> bool {
    let slots = covered_slots(subscribe, &candidate.release);
    // H1: at most one upgrade per owned slot in flight. A candidate is only
    // worth taking for slots that are neither owned-with-upgrade-pending nor
    // otherwise blocked; if every slot it touches already has an in-flight
    // upgrade, skip it (the in-flight download decides once it lands).
    if subscribe.wash_cut
        && !slots.is_empty()
        && slots.iter().all(|(s, e)| facts.is_in_flight(*s, *e))
    {
        return false;
    }
    let open: Vec<_> = slots
        .iter()
        .copied()
        .filter(|(s, e)| !facts.is_in_flight(*s, *e))
        .collect();
    should_replace_slots(subscribe, wash_filter, facts, candidate, &open)
}

/// 逐槽位判定候选是否应替换已有事实。choose() 用它做选择；collect.rs 用它
/// 决定事实是否无条件覆盖——两个阶段必须共用同一把尺。
pub(crate) fn should_replace_slots(
    subscribe: &Subscribe,
    wash_filter: Option<&domain::Filter>,
    facts: &SubscribeFacts,
    candidate: &ScoredTorrent,
    slots: &[(Option<u32>, Option<u32>)],
) -> bool {
    if slots.is_empty() {
        return false;
    }
    slots
        .iter()
        .any(|(season, episode)| match facts.get(*season, *episode) {
            None => true,
            Some(existing) => {
                if !subscribe.wash_cut {
                    return false;
                }
                let old_quality = existing.path.as_deref().and_then(|path| owned_quality(facts, path));
                // T5: cutoff 与 ladder 排序分离：
                // 只有 target 显式声明的维度全部满足时才算已达标停止升级；
                // UpgradeLadder 仅用于比对候选优劣，不替代达标判断。
                if let Some(target_value) = wash_target_value(wash_filter) {
                    let target_release = release_from_target_value(&target_value);
                    if target_release.resolution.is_none()
                        && target_release.source.is_none()
                        && target_release.codec.is_none()
                        && target_release.hdr.is_none()
                    {
                        tracing::error!(target_value = %target_value, "WashTarget 配置非法或无法解析有效维度");
                    } else {
                        if let Some(existing_q) = old_quality.as_ref() {
                            if target_reached(existing_q, &target_release) {
                                return false;
                            }
                        }
                    }
                    // 无法确定现有质量时继续尝试升级
                }
                // 旧质量未知不能授权删除。有阶梯时只比较已知维度；
                // 没有阶梯时，分数只能比较两个已知质量。
                match ladder_for(wash_filter) {
                    Some(ladder) => {
                        old_quality.is_some_and(|old| {
                            ladder_compare(&candidate.release, &old, &ladder)
                                == Some(std::cmp::Ordering::Greater)
                        })
                    }
                    None => {
                        let Some(old) = old_quality.as_ref().filter(|q| quality_known(q)) else {
                            tracing::warn!(path = ?existing.path, "已有质量未知，拒绝按分数批准 Wash-cut");
                            return false;
                        };
                        // M9: built-in default ladder; score only breaks ties
                        // between equal (comparable) quality.
                        match default_ladder_compare(&candidate.release, old) {
                            std::cmp::Ordering::Greater => true,
                            std::cmp::Ordering::Equal => candidate.score > existing.score,
                            std::cmp::Ordering::Less => false,
                        }
                    }
                }
            }
        })
}

/// 返回 WashTarget atom 的 value 字符串（如 "2160p web-dl"）。
/// D06: 不再返回 priority 数字，而是返回目标质量描述，以便做维度比较。
fn wash_target_value(wash_filter: Option<&domain::Filter>) -> Option<String> {
    let filter = wash_filter?;
    filter.atoms.iter().find_map(|a| match &a.rule {
        domain::AtomRule::WashTarget(v) => Some(v.clone()),
        _ => None,
    })
}

/// 把 WashTarget value 字符串 parse 成伪 Release，用于维度比较。
fn release_from_target_value(value: &str) -> domain::Release {
    release::parse(value)
}

/// 检查 owned 是否已达到 target 声明的截止目标。
/// 只有 target 显式声明的维度参与比较（要求非空）；
/// 对于每个声明的维度，owned 必须已知且等级 >= 目标的等级。
pub fn target_reached(owned: &Release, target: &Release) -> bool {
    let mut specified = 0;

    if let Some(target_res) = &target.resolution {
        specified += 1;
        let Some(owned_res) = &owned.resolution else {
            return false;
        };
        if resolution_level(owned_res) < resolution_level(target_res) {
            return false;
        }
    }

    if let Some(target_src) = &target.source {
        specified += 1;
        let Some(owned_src) = &owned.source else {
            return false;
        };
        if source_level(owned_src) < source_level(target_src) {
            return false;
        }
    }

    if let Some(target_codec) = &target.codec {
        specified += 1;
        let Some(owned_codec) = &owned.codec else {
            return false;
        };
        if codec_level(owned_codec) < codec_level(target_codec) {
            return false;
        }
    }

    if let Some(target_hdr) = &target.hdr {
        specified += 1;
        let Some(owned_hdr) = &owned.hdr else {
            return false;
        };
        if hdr_level(owned_hdr) < hdr_level(target_hdr) {
            return false;
        }
    }

    // specified target dimensions must be nonempty
    if specified == 0 {
        return false;
    }

    true
}

/// Ordered Wash-cut comparison dimensions configured on the Filter.
/// L3: dimensions an `UpgradeLadder` may name.
pub const LADDER_DIMENSIONS: [&str; 4] = ["resolution", "source", "codec", "hdr"];

/// L3: the first ladder dimension that is not in `LADDER_DIMENSIONS`.
pub fn unknown_ladder_dimension(raw: &str) -> Option<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|dim| !dim.is_empty())
        .find(|dim| !LADDER_DIMENSIONS.contains(dim))
        .map(str::to_string)
}

fn ladder_for(wash_filter: Option<&domain::Filter>) -> Option<Vec<String>> {
    let filter = wash_filter?;
    filter.atoms.iter().find_map(|atom| match &atom.rule {
        domain::AtomRule::UpgradeLadder(raw) => Some(
            raw.split(',')
                .map(|dim| dim.trim().to_string())
                .filter(|dim| !dim.is_empty())
                .collect::<Vec<_>>(),
        ),
        _ => None,
    })
}

/// Probe/persisted dimensions win; a filename only fills absent dimensions.
fn owned_quality(facts: &SubscribeFacts, path: &str) -> Option<Release> {
    let stored = facts.quality(path).cloned();
    let parsed = std::path::Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .map(release::parse)
        .filter(quality_known);
    match (stored, parsed) {
        (Some(mut stored), Some(parsed)) => {
            stored.resolution = stored.resolution.or(parsed.resolution);
            stored.source = stored.source.or(parsed.source);
            stored.codec = stored.codec.or(parsed.codec);
            stored.hdr = stored.hdr.or(parsed.hdr);
            Some(stored)
        }
        (stored, parsed) => stored.or(parsed),
    }
}

fn quality_known(release: &Release) -> bool {
    release.resolution.is_some()
        || release.source.is_some()
        || release.codec.is_some()
        || release.hdr.is_some()
}

fn ladder_compare(new: &Release, old: &Release, ladder: &[String]) -> Option<std::cmp::Ordering> {
    let mut result = std::cmp::Ordering::Equal;
    for dim in ladder {
        let (new_value, old_value, level): (_, _, fn(&str) -> i32) = match dim.as_str() {
            "resolution" => (&new.resolution, &old.resolution, resolution_level),
            "source" => (&new.source, &old.source, source_level),
            "codec" => (&new.codec, &old.codec, codec_level),
            "hdr" => (&new.hdr, &old.hdr, hdr_level),
            other => {
                // L3: saved ladders are validated; an old row may still hold a
                // bad name. Report it once instead of silently ignoring it.
                static REPORTED: std::sync::Once = std::sync::Once::new();
                REPORTED.call_once(|| {
                    tracing::error!(dimension = %other, "UpgradeLadder 含未知维度，已忽略");
                });
                continue;
            }
        };
        match (new_value.as_deref(), old_value.as_deref()) {
            (Some(new_value), Some(old_value)) => {
                let ordering = level(new_value).cmp(&level(old_value));
                if result == std::cmp::Ordering::Equal {
                    result = ordering;
                }
            }
            _ => {
                tracing::warn!(dimension = %dim, "Wash-cut 维度质量未知，不能据此批准替换");
                return None;
            }
        }
    }
    Some(result)
}

/// M9: default `resolution,hdr,codec,source` ordering used when no
/// UpgradeLadder is configured. Dimensions unknown on either side are skipped;
/// any known downgrade makes the candidate worse.
fn default_ladder_compare(new: &Release, old: &Release) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let dims: [(Option<&str>, Option<&str>, fn(&str) -> i32); 4] = [
        (
            new.resolution.as_deref(),
            old.resolution.as_deref(),
            resolution_level,
        ),
        (new.hdr.as_deref(), old.hdr.as_deref(), hdr_level),
        (new.codec.as_deref(), old.codec.as_deref(), codec_level),
        (new.source.as_deref(), old.source.as_deref(), source_level),
    ];
    let mut result = Ordering::Equal;
    for (new_value, old_value, level) in dims {
        let (Some(new_value), Some(old_value)) = (new_value, old_value) else {
            continue;
        };
        match level(new_value).cmp(&level(old_value)) {
            Ordering::Less => return Ordering::Less,
            Ordering::Greater if result == Ordering::Equal => result = Ordering::Greater,
            _ => {}
        }
    }
    result
}

fn resolution_level(value: &str) -> i32 {
    match value.to_ascii_lowercase().as_str() {
        "2160p" | "4k" | "uhd" => 4,
        "1080p" | "1080i" => 3,
        "720p" => 2,
        "480p" | "576p" => 1,
        _ => 0,
    }
}

fn source_level(value: &str) -> i32 {
    match value.to_ascii_lowercase().as_str() {
        "remux" => 6,
        "bluray" | "blu-ray" => 5,
        "web-dl" | "webdl" => 4,
        "webrip" => 3,
        "hdtv" => 2,
        "dvdrip" | "cam" | "ts" => 1,
        _ => 0,
    }
}

/// Without an explicit ladder, title/site score cannot authorize a physical
/// downgrade. Unknown probed dimensions are not evidence for deleting a known file.
pub(crate) fn collected_quality_is_safe(
    filter: &domain::Filter,
    facts: &SubscribeFacts,
    candidate: &Release,
    slots: &[(Option<u32>, Option<u32>)],
) -> bool {
    if ladder_for(Some(filter)).is_some() {
        return true; // explicit dimension ordering is enforced by should_replace_slots
    }
    slots.iter().all(|(s, e)| {
        let Some(existing) = facts.get(*s, *e) else {
            return true;
        };
        let Some(old) = existing
            .path
            .as_deref()
            .and_then(|p| owned_quality(facts, p))
        else {
            return false;
        };
        [
            (
                old.resolution.as_deref(),
                candidate.resolution.as_deref(),
                resolution_level as fn(&str) -> i32,
            ),
            (
                old.codec.as_deref(),
                candidate.codec.as_deref(),
                codec_level,
            ),
            (old.hdr.as_deref(), candidate.hdr.as_deref(), hdr_level),
        ]
        .into_iter()
        .all(|(owned, incoming, rank)| {
            owned.is_none_or(|owned| incoming.is_some_and(|incoming| rank(incoming) >= rank(owned)))
        })
    })
}

fn codec_level(value: &str) -> i32 {
    match value.to_ascii_lowercase().as_str() {
        "av1" => 4,
        "hevc" | "x265" | "h.265" | "h265" => 3,
        "x264" | "h.264" | "h264" | "avc" => 2,
        _ => 0,
    }
}

fn hdr_level(value: &str) -> i32 {
    match value.to_ascii_lowercase().as_str() {
        "dv" | "dovi" | "dolbyvision" => 3,
        "hdr10+" => 2,
        "hdr10" | "hdr" => 1,
        _ => 0,
    }
}

pub(crate) fn covered_slots(
    subscribe: &Subscribe,
    release: &Release,
) -> Vec<(Option<u32>, Option<u32>)> {
    match subscribe.coverage {
        Coverage::Movie => vec![(None, None)],
        Coverage::Tv {
            season,
            episode_from,
            episode_to,
        } => {
            let Some(rel_season) = release.season else {
                return Vec::new();
            };
            if rel_season != season {
                return Vec::new();
            }
            let (from, to) = match release.episode_span() {
                Some(span) => span,
                None if subscribe.full_season_pack => {
                    let pack_to = episode_to.unwrap_or(episode_from);
                    let capped =
                        episode_from.saturating_add(Coverage::MAX_EPISODES.saturating_sub(1));
                    (episode_from, pack_to.min(capped))
                }
                None => return Vec::new(),
            };
            let window_to =
                episode_to.unwrap_or(from.saturating_add(Coverage::MAX_EPISODES.saturating_sub(1)));
            (from.max(episode_from)..=to.min(window_to))
                .map(|ep| (Some(season), Some(ep)))
                .collect()
        }
    }
}

/// Candidates from automatic search, manual delivery, and persisted pending
/// rows must share the Subscribe's Media identity and coverage.
pub fn candidate_matches_subscribe(
    subscribe: &Subscribe,
    media: &Media,
    release: &Release,
) -> bool {
    let keywords = short_title_keyword(media);
    candidate_matches_subscribe_with_keywords(subscribe, media, release, &keywords)
}

fn short_title_keyword(media: &Media) -> Vec<String> {
    let short = media
        .title
        .split(['：', ':', '（', '(', '「', '『'])
        .next()
        .unwrap_or(&media.title)
        .trim();
    if short.is_empty() || short == media.title {
        Vec::new()
    } else {
        vec![short.to_string()]
    }
}

pub(crate) fn candidate_matches_subscribe_with_keywords(
    subscribe: &Subscribe,
    media: &Media,
    release: &Release,
    search_keywords: &[String],
) -> bool {
    if subscribe.media_id != media.id || !matches_media(media, release, search_keywords) {
        return false;
    }
    release_matches_subscribe(subscribe, media, release)
}

/// Identity check for an in-flight pending release (H4): like
/// `candidate_matches_subscribe`, but a pure season pack of the subscribed
/// season counts even when the chooser would not pick packs itself (a pack
/// admitted by hand still occupies the season).
pub(crate) fn pending_matches_subscribe(
    subscribe: &Subscribe,
    media: &Media,
    release: &Release,
) -> bool {
    if candidate_matches_subscribe(subscribe, media, release) {
        return true;
    }
    let Coverage::Tv { season, .. } = subscribe.coverage else {
        return false;
    };
    let keywords = short_title_keyword(media);
    subscribe.media_id == media.id
        && matches_media(media, release, &keywords)
        && release.season == Some(season)
        && release.covered_episodes().is_empty()
}

fn release_matches_subscribe(subscribe: &Subscribe, media: &Media, release: &Release) -> bool {
    if let (Some(want), Some(got)) = (media.year, release.year) {
        if want != got {
            return false;
        }
    }
    match (&subscribe.coverage, media.kind) {
        (Coverage::Movie, MediaKind::Movie) => {
            release.season.is_none() && release.episode.is_none() && release.episode_to.is_none()
        }
        // G-H2: a defaulted S01 never stands in for another season.
        (Coverage::Tv { season, .. }, MediaKind::Tv) if release.season_inferred && *season != 1 => {
            false
        }
        (Coverage::Tv { .. }, MediaKind::Tv) => !covered_slots(subscribe, release).is_empty(),
        _ => false,
    }
}

/// L1: average size of admitted single-episode candidates, when known.
fn single_episode_average_size(admitted: &[ScoredTorrent]) -> Option<u64> {
    let sizes: Vec<u64> = admitted
        .iter()
        .filter(|c| matches!(c.release.episode_span(), Some((from, to)) if from == to))
        .filter_map(|c| c.torrent.size_bytes)
        .collect();
    (!sizes.is_empty()).then(|| sizes.iter().sum::<u64>() / sizes.len() as u64)
}

/// L1: a bare `S01` (no episode numbers) only counts as a season pack when
/// its size is at least half of `average single × episodes`. Without a size
/// or without single-episode references the old behaviour is kept.
fn pack_size_plausible(
    candidate: &ScoredTorrent,
    subscribe: &Subscribe,
    average: Option<u64>,
) -> bool {
    if candidate.release.episode.is_some() {
        return true;
    }
    let (Some(size), Some(average)) = (candidate.torrent.size_bytes, average) else {
        return true;
    };
    let Coverage::Tv {
        episode_from,
        episode_to: Some(episode_to),
        ..
    } = subscribe.coverage
    else {
        return true;
    };
    let episodes = u64::from(episode_to.saturating_sub(episode_from) + 1);
    let plausible = size.saturating_mul(2) >= average.saturating_mul(episodes);
    if !plausible {
        tracing::debug!(torrent = %candidate.torrent.title, size, average, episodes, "纯季包体积过小，不视为整季包");
    }
    plausible
}

fn is_pack(release: &Release, subscribe: &Subscribe) -> bool {
    let Coverage::Tv {
        season,
        episode_from,
        episode_to,
    } = subscribe.coverage
    else {
        return false;
    };
    let Some(episode_to) = episode_to else {
        return false;
    };
    if release.season != Some(season) {
        return false;
    }
    // 明确标注了范围 E01-E10 的整季包，或者省略集号的 S01 纯季包
    // L2: an explicit range pack is accepted when it covers the whole window
    // (E01-E13 for E01-E12); episodes outside the window are not imported.
    matches!((release.episode, release.episode_to), (Some(from), Some(to)) if from <= episode_from && to >= episode_to)
        || (release.episode.is_none() && release.episode_to.is_none())
}

/// Whether a bounded, non-wash subscription owns every unit.
///
/// M4b: wash-cut and open-window subscriptions are never "complete" here by
/// design: they keep tracking upgrades / future episodes. The persisted
/// `completed` tracking state (M4a, `api::completion`) applies only to
/// bounded, non-wash coverage.
pub fn is_complete(subscribe: &Subscribe, facts: &SubscribeFacts) -> bool {
    if subscribe.wash_cut {
        return false;
    }
    match subscribe.coverage {
        Coverage::Movie => facts.movie().is_some(),
        Coverage::Tv {
            season,
            episode_from,
            episode_to,
        } => {
            let Some(to) = episode_to else {
                return false;
            };
            let capped = episode_from.saturating_add(Coverage::MAX_EPISODES.saturating_sub(1));
            (episode_from..=to.min(capped)).all(|ep| facts.get(Some(season), Some(ep)).is_some())
        }
    }
}

pub(crate) fn matches_media(
    media: &domain::Media,
    release: &Release,
    search_keywords: &[String],
) -> bool {
    if crate::media_identity::media_title_matches(media, &release.title) {
        return true;
    }
    // 候选关键词匹配（包含搜索生成器计算的各种衍生变体与年份组合）
    for kw in search_keywords {
        if crate::media_identity::title_matches(kw, &release.title) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests;
