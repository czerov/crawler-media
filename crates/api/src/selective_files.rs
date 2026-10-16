//! F-1: when a pack is chosen only to fill some missing episodes, ask the
//! client to skip the files of episodes already owned or in flight.
//! Off unless `subscribe.selective_files` is `true`.

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
pub(crate) fn keep_file(name: &str, missing: &BTreeSet<(u32, u32)>) -> bool {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let parsed = release::parse(base);
    let units = parsed.covered_episodes();
    units.is_empty() || units.iter().any(|unit| missing.contains(unit))
}

pub(crate) fn apply<D: Downloader + ?Sized>(
    client: &D,
    subscribe: &domain::Subscribe,
    chosen: &[filter::ScoredTorrent],
    facts: &subscribe::SubscribeFacts,
) {
    for scored in chosen {
        let Some(missing) = missing_episodes(subscribe, &scored.release, facts) else {
            continue;
        };
        match client.select_files(&scored.torrent, &|name| keep_file(name, &missing)) {
            Ok(0) => {
                tracing::info!(torrent = %scored.torrent.title, "选择性下载：文件列表未就绪或无需跳过，整包下载")
            }
            Ok(skipped) => {
                tracing::info!(torrent = %scored.torrent.title, skipped, "选择性下载：已跳过已有集的文件")
            }
            Err(error) => {
                tracing::warn!(torrent = %scored.torrent.title, %error, "选择性下载设置失败，整包下载")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_missing_and_unknown_files() {
        let missing = BTreeSet::from([(1, 2)]);
        assert!(keep_file("Pack/Show.S01E02.1080p.mkv", &missing));
        assert!(!keep_file("Pack/Show.S01E01.1080p.mkv", &missing));
        assert!(keep_file("Pack/Show.nfo", &missing));
    }
}
