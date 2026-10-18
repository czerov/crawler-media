//! F-1: when a pack is chosen only to fill some missing episodes, ask the
//! client to skip the files of episodes already owned or in flight.
//! Controlled by the global setting `subscribe.selective_files` (applies to
//! every subscription; there is no per-subscription switch). Off unless it
//! is `true`.

use std::collections::BTreeSet;

use downloader::Downloader;

use crate::ApiState;

pub(crate) fn enabled(state: &ApiState) -> bool {
    state
        .store
        .lock()
        .get_setting(crate::settings_keys::SUBSCRIBE_SELECTIVE_FILES)
        .ok()
        .flatten()
        .is_some_and(|value| matches!(value.trim(), "true" | "1" | "on"))
}

/// Episodes of `release` that `facts` (owned + in flight before this run) lacks.
/// `None` when nothing should be deselected (movie, pure upgrade, all missing).
pub(crate) fn missing_episodes(
    subscribe: &domain::Subscribe,
    release: &domain::Release,
    facts: &subscribe::SubscribeFacts,
) -> Option<BTreeSet<(u32, u32)>> {
    let episodes: Vec<(u32, u32)> = subscribe::pending_slots(subscribe, release)
        .into_iter()
        .filter_map(|slot| match slot {
            (Some(season), Some(episode)) => Some((season, episode)),
            _ => None,
        })
        .collect();
    let missing: BTreeSet<_> = episodes
        .iter()
        .copied()
        .filter(|(s, e)| facts.get(Some(*s), Some(*e)).is_none())
        .collect();
    (!missing.is_empty() && missing.len() < episodes.len()).then_some(missing)
}

/// Keep a file unless it names only episodes outside `missing`. Files without a
/// recognisable episode (nfo, extras) are kept.
///
/// Review H-2: a file name without a season marker (`Show - E03.mkv`) parses
/// with the default season 1 (`season_inferred`). Such a file inherits the
/// season of its folder when the folder names one, else the subscribed season
/// (the season of `missing`), so a season-2 pack is not deselected wholesale.
///
/// Review M-6: without a folder season an absolute-numbered file
/// (`Show - 15.mkv`) is mapped onto the catalog seasons first; if that is not
/// possible and `missing` spans several seasons the file is kept.
pub(crate) fn keep_file(
    name: &str,
    missing: &BTreeSet<(u32, u32)>,
    absolute: Option<&subscribe::AbsoluteEpisodes>,
) -> bool {
    let mut parts: Vec<&str> = name.split(['/', '\\']).collect();
    let base = parts.pop().unwrap_or(name);
    let parsed = release::parse(base);
    let mut units = parsed.covered_episodes();
    if parsed.season_inferred {
        let folder_season = parts
            .iter()
            .rev()
            .map(|dir| release::parse(dir))
            .find(|dir| dir.season.is_some() && !dir.season_inferred)
            .and_then(|dir| dir.season);
        if folder_season.is_none()
            && let Some(map) = absolute
        {
            match map.remap(&parsed) {
                Ok(Some(mapped)) => {
                    let mapped = mapped.covered_episodes();
                    return mapped.is_empty() || mapped.iter().any(|unit| missing.contains(unit));
                }
                Err(_) => return true,
                Ok(None) => {}
            }
        }
        let seasons: BTreeSet<u32> = missing.iter().map(|(s, _)| *s).collect();
        let season = match (folder_season, seasons.len()) {
            (Some(season), _) => season,
            (None, 1) => *seasons.iter().next().expect("one season"),
            _ => return true,
        };
        for unit in &mut units {
            unit.0 = season;
        }
    }
    units.is_empty() || units.iter().any(|unit| missing.contains(unit))
}

fn encode(missing: &BTreeSet<(u32, u32)>) -> String {
    missing
        .iter()
        .map(|(s, e)| format!("{s}:{e}"))
        .collect::<Vec<_>>()
        .join(",")
}

fn decode(raw: &str) -> BTreeSet<(u32, u32)> {
    raw.split(',')
        .filter_map(|pair| {
            let (s, e) = pair.split_once(':')?;
            Some((s.trim().parse().ok()?, e.trim().parse().ok()?))
        })
        .collect()
}

/// Apply selection right after submit. Returns `(enclosure, encoded missing)`
/// for tasks whose file list was not known yet, to be retried by reconcile.
pub(crate) fn apply<D: Downloader + ?Sized>(
    client: &D,
    subscribe: &domain::Subscribe,
    chosen: &[filter::ScoredTorrent],
    facts: &subscribe::SubscribeFacts,
    absolute: Option<&subscribe::AbsoluteEpisodes>,
) -> Vec<(String, String)> {
    let mut deferred = Vec::new();
    for scored in chosen {
        let Some(missing) = missing_episodes(subscribe, &scored.release, facts) else {
            continue;
        };
        // M-6: same rule as collect: only titles without a season marker.
        let absolute = absolute.filter(|_| !release::has_explicit_season(&scored.torrent.title));
        match client.select_files(&scored.torrent, &|name| keep_file(name, &missing, absolute)) {
            Ok(None) => {
                tracing::info!(torrent = %scored.torrent.title, "选择性下载：文件列表未就绪，待对账时重试");
                deferred.push((scored.torrent.enclosure.clone(), encode(&missing)));
            }
            Ok(Some(skipped)) => {
                tracing::info!(torrent = %scored.torrent.title, skipped, "选择性下载：已跳过已有集的文件")
            }
            Err(error) => {
                tracing::warn!(torrent = %scored.torrent.title, %error, "选择性下载设置失败，整包下载")
            }
        }
    }
    deferred
}

/// Reconcile-time retry for deferred selections (metadata arrived later).
/// A row is cleared once the client reports its file list.
pub(crate) fn retry_deferred(
    state: &ApiState,
    subscribe_id: domain::SubscribeId,
    pending: &[(i32, crate::store::PendingDownload)],
) {
    let deferred = match state.store.lock().load_pending_selective(subscribe_id) {
        Ok(map) if !map.is_empty() => map,
        _ => return,
    };
    if !enabled(state) {
        return;
    }
    let hashes = state
        .store
        .lock()
        .load_pending_info_hashes(subscribe_id)
        .unwrap_or_default();
    let media = {
        let store = state.store.lock();
        store
            .get_subscribe(subscribe_id)
            .ok()
            .flatten()
            .and_then(|sub| store.get_media(sub.media_id).ok().flatten())
    };
    for (_, item) in pending {
        let Some(raw) = deferred.get(&item.torrent.enclosure) else {
            continue;
        };
        let missing = decode(raw);
        let Ok(client) = crate::delivery::client_for_id(state, item.downloader_id) else {
            continue;
        };
        if let Some(hash) = hashes.get(&item.torrent.enclosure) {
            let _ = client.remember_identity(&item.torrent, hash);
        }
        let absolute = media
            .as_ref()
            .filter(|_| !release::has_explicit_season(&item.torrent.title))
            .and_then(|media| {
                crate::season_check::absolute_map(state, media, [item.torrent.title.as_str()])
            });
        match client.select_files(&item.torrent, &|name| {
            keep_file(name, &missing, absolute.as_ref())
        }) {
            Ok(None) => {}
            Ok(Some(skipped)) => {
                tracing::info!(torrent = %item.torrent.title, skipped, "选择性下载：元数据到达后已跳过已有集的文件");
                if let Err(error) = state.store.lock().set_pending_selective(
                    subscribe_id,
                    &item.torrent.enclosure,
                    None,
                ) {
                    tracing::warn!(%error, "清除待选择文件标记失败");
                }
            }
            Err(error) => {
                tracing::warn!(torrent = %item.torrent.title, %error, "选择性下载重试失败")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Review M-6: absolute numbers map onto the catalog season first.
    #[test]
    fn absolute_numbered_files_are_mapped_before_selection() {
        let map = subscribe::AbsoluteEpisodes::new(vec![(1, 12), (2, 12)]);
        let missing = BTreeSet::from([(2, 3)]);
        assert!(
            keep_file("Pack/Show Episode 15 1080p.mkv", &missing, Some(&map)),
            "15 = S02E03"
        );
        assert!(
            !keep_file("Pack/Show Episode 14 1080p.mkv", &missing, Some(&map)),
            "14 = S02E02 owned"
        );
        assert!(
            keep_file("Pack/Show Episode 99 1080p.mkv", &missing, Some(&map)),
            "unmappable: keep"
        );
    }

    #[test]
    fn keeps_missing_and_unknown_files() {
        let missing = BTreeSet::from([(1, 2)]);
        assert!(keep_file("Pack/Show.S01E02.1080p.mkv", &missing, None));
        assert!(!keep_file("Pack/Show.S01E01.1080p.mkv", &missing, None));
        assert!(keep_file("Pack/Show.nfo", &missing, None));
        // H-2: season-less names inherit the folder / subscribed season.
        let s2 = BTreeSet::from([(2, 3)]);
        assert!(keep_file("Show.S02/Show - E03.mkv", &s2, None));
        assert!(keep_file("Pack/Show - E03.mkv", &s2, None));
        assert!(!keep_file("Pack/Show - E01.mkv", &s2, None));
        let set = BTreeSet::from([(1, 2), (2, 10)]);
        // M-6: missing spans two seasons and no folder season: keep.
        assert!(keep_file("Pack/Show - E03.mkv", &set, None));
        assert_eq!(decode(&encode(&set)), set);
    }
}
