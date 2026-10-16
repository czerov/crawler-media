//! G-H1: skip scheduled keyword searches while every still-wanted episode of a
//! TV Subscribe has a known air date in the future. Unknown air dates, an
//! empty catalog, or a disabled setting keep the old always-search behavior.

use std::collections::HashMap;

use domain::{Coverage, Media, MediaKind, Subscribe};

use crate::management::ApiState;

/// True when the indexer should not be queried this round.
pub(crate) fn all_wanted_unaired(state: &ApiState, subscribe: &Subscribe, media: &Media) -> bool {
    let (MediaKind::Tv, Some(tmdb_id), Coverage::Tv { season, .. }) =
        (media.kind, media.tmdb_id.as_deref(), &subscribe.coverage)
    else {
        return false;
    };
    let Some(wanted) = wanted_units(state, subscribe, media) else {
        return false;
    };
    if wanted.is_empty() {
        return false;
    }
    // Store lock must be released before calling the catalog (deadlock, see http/subscriptions.rs).
    let air_dates: HashMap<u32, Option<String>> = match state
        .catalog
        .season_episodes(tmdb_id, *season)
    {
        Ok(episodes) => episodes
            .into_iter()
            .map(|ep| (ep.episode_number, ep.air_date))
            .collect(),
        Err(err) => {
            tracing::debug!(subscribe_id = %subscribe.id, error = %err, "air-date gate: catalog unavailable");
            return false;
        }
    };
    // Air dates are local calendar dates, so compare against the server's local date.
    let today = crate::compact_time::local_date(crate::job_loop::unix_now());
    let today = today.as_str();
    // Episodes beyond the last one the catalog lists (open-window frontier)
    // are not announced yet, so they count as unaired too.
    let Some(last_listed) = air_dates.keys().max().copied() else {
        return false;
    };
    let unaired = wanted.iter().all(|(_, episode)| {
        let Some(episode) = episode else { return false };
        match air_dates.get(episode) {
            Some(Some(date)) => is_future(date, today),
            Some(None) => false,
            None => *episode > last_listed,
        }
    });
    if unaired {
        tracing::info!(subscribe_id = %subscribe.id, wanted = wanted.len(), "air-date gate: all wanted episodes unaired, skipping site search");
    }
    unaired
}

fn is_future(air_date: &str, today: &str) -> bool {
    air_date.len() >= 10 && air_date.get(..10).is_some_and(|d| d > today)
}

fn wanted_units(
    state: &ApiState,
    subscribe: &Subscribe,
    media: &Media,
) -> Option<Vec<(Option<u32>, Option<u32>)>> {
    let store = state.store.lock();
    if let Ok(Some(value)) = store.get_setting(crate::settings_keys::SUBSCRIBE_AIR_DATE_GATE) {
        if matches!(value.trim(), "0" | "false" | "off") {
            return None;
        }
    }
    let facts = store
        .load_library_subscribe_facts(subscribe, media.kind)
        .ok()?;
    let pending = store.load_pending(subscribe.id).ok()?;
    let history = store.load_wanted_history(subscribe.id).ok()?;
    drop(store);
    let releases = crate::worker::finish_search::pending_releases(&pending);
    let facts = subscribe::facts_with_pending(facts, &releases, subscribe, media);
    let units = crate::open_coverage::units(
        subscribe,
        media,
        &facts,
        &pending,
        &history,
        std::iter::empty(),
    );
    Some(
        units
            .into_iter()
            .filter(|(s, e)| facts.get(*s, *e).is_none())
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn future_compare() {
        assert!(super::is_future("2026-10-20", "2026-10-16"));
        assert!(!super::is_future("2026-10-16", "2026-10-16"));
        assert!(!super::is_future("", "2026-10-16"));
    }
}
