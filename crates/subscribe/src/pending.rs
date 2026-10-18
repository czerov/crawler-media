//! One shared rule for how in-flight pending downloads placehold slots (H4),
//! used by both the scheduled search tail and the manual "run now" path.

use domain::{Coverage, Media, Release, Subscribe};

use crate::choose::pending_matches_subscribe;
use crate::{QualityFact, SubscribeFacts};

/// Slots an in-flight `release` occupies for `subscribe`.
///
/// A pure season pack (season, no episodes) of the subscribed season covers
/// every episode of the subscribed window, regardless of `full_season_pack`
/// (a manually admitted pack must block per-episode downloads too).
pub fn pending_slots(subscribe: &Subscribe, release: &Release) -> Vec<(Option<u32>, Option<u32>)> {
    if release.season.is_none() {
        return vec![(None, None)];
    }
    let units = release.covered_episodes();
    if !units.is_empty() {
        return units.into_iter().map(|(s, e)| (Some(s), Some(e))).collect();
    }
    if let Coverage::Tv { season, episode_from, episode_to } = subscribe.coverage
        && release.season == Some(season)
    {
        let capped = episode_from.saturating_add(Coverage::MAX_EPISODES.saturating_sub(1));
        let pack_to = episode_to.unwrap_or(episode_from).min(capped);
        let mut slots: Vec<_> = (episode_from..=pack_to).map(|ep| (Some(season), Some(ep))).collect();
        slots.push((Some(season), None));
        return slots;
    }
    vec![(release.season, None)]
}

/// Overlay in-flight pending releases onto owned `facts`: empty slots get a
/// score-0 placeholder, owned slots are marked as in-flight upgrades (H1).
pub fn facts_with_pending<'a>(
    mut facts: SubscribeFacts,
    pending: impl IntoIterator<Item = &'a Release>,
    subscribe: &Subscribe,
    media: &Media,
) -> SubscribeFacts {
    for release in pending {
        if !pending_matches_subscribe(subscribe, media, release) {
            continue;
        }
        for (season, episode) in pending_slots(subscribe, release) {
            match facts.get(season, episode) {
                Some(fact) if fact.path.is_some() => facts.mark_in_flight(season, episode),
                Some(_) => {}
                None => facts.upsert(season, episode, QualityFact { score: 0, path: None }),
            }
        }
    }
    facts
}
