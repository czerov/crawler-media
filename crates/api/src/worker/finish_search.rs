use subscribe::{RunInput, admit_and_add};

use super::{notify, notify_subscription_complete, record_wanted_round};
use crate::management::ApiState;
use crate::scrape_store::ScrapeStoreExt;

/// Shared tail of every subscribe search: load facts, admit + add to the
/// downloader, record pending, notify.
pub(super) fn finish_search(
    state: &ApiState,
    initial_subscribe: &domain::Subscribe,
    mut torrents: Vec<domain::Torrent>,
    search_keywords: Vec<String>,
) -> Result<(), String> {
    let guard = state.subscribe_guard(initial_subscribe.id);
    let _guard = guard.lock();
    if state.subscribe_is_deleting(initial_subscribe.id)
        || !subscribe_is_active(state, initial_subscribe.id)?
    {
        tracing::info!(subscribe_id = %initial_subscribe.id, "订阅在搜索期间已暂停或删除，跳过下载提交");
        return Ok(());
    }

    // Reload authoritative Subscribe, Media, and Filter within the critical section
    let (subscribe, media, filter) = {
        let store = state.store.lock();
        let Some(subscribe) = store
            .get_subscribe(initial_subscribe.id)
            .map_err(|e| e.to_string())?
        else {
            tracing::info!(subscribe_id = %initial_subscribe.id, "订阅已不存在，跳过下载提交");
            return Ok(());
        };
        let media = store
            .get_media(subscribe.media_id)
            .map_err(|e| e.to_string())?
            .ok_or("Media not found")?;
        let filter = store
            .get_filter(subscribe.filter_id)
            .map_err(|e| e.to_string())?
            .ok_or("Filter not found")?;
        (subscribe, media, filter)
    };

    let context = load_search_context(state, &subscribe, &media)?;
    tracing::info!(
        subscribe_id = %subscribe.id,
        media = %media.title,
        candidates = torrents.len(),
        "评估候选种子"
    );
    remove_pending_candidates(&mut torrents, &context.existing, subscribe.id);
    // H2: never auto-pick an enclosure whose probed quality already vetoed it.
    let rejected = state
        .store
        .lock()
        .load_rejected_enclosures(subscribe.id)
        .map_err(|e| e.to_string())?;
    if !rejected.is_empty() {
        torrents.retain(|torrent| !rejected.contains(&torrent.enclosure));
    }

    let add_errors = admit_candidates(
        state,
        &subscribe,
        &media,
        &filter,
        &context,
        torrents,
        search_keywords,
    )?;
    drop(_guard);
    if !add_errors.is_empty() {
        return Err(format!(
            "{} torrent submission failure(s): {}",
            add_errors.len(),
            add_errors.join("; ")
        ));
    }
    Ok(())
}

struct SearchContext {
    facts: subscribe::SubscribeFacts,
    wash_filter: Option<domain::Filter>,
    existing: Vec<(i32, crate::store::PendingDownload)>,
    library_root: std::path::PathBuf,
    transfer_mode: Option<library::TransferMode>,
    scrape: bool,
    naming: String,
}

fn load_search_context(
    state: &ApiState,
    subscribe: &domain::Subscribe,
    media: &domain::Media,
) -> Result<SearchContext, String> {
    let (facts, wash_filter) = {
        let store = state.store.lock();
        let facts = store
            .load_library_subscribe_facts(subscribe, media.kind)
            .map_err(|error| error.to_string())?;
        let wash_filter = subscribe
            .wash_cut_filter_id
            .and_then(|id| store.get_filter(id).ok().flatten())
            .or_else(|| store.get_filter(subscribe.filter_id).ok().flatten());
        (facts, wash_filter)
    };
    let (library_root, transfer_mode, scrape) =
        crate::directory::transfer_plan_for_library(
            &state.store.lock(), media.kind,
            subscribe.library_id.as_ref().map(|id| id.to_string()).as_deref(),
        )
            .map_err(|error| error.to_string())?;
    let naming = state
        .store
        .lock()
        .naming_pattern(media.kind)
        .map_err(|error| error.to_string())?;
    let existing = state
        .store
        .lock()
        .load_pending(subscribe.id)
        .map_err(|error| error.to_string())?;
    Ok(SearchContext {
        facts,
        wash_filter,
        existing,
        library_root,
        transfer_mode,
        scrape,
        naming,
    })
}

fn remove_pending_candidates(
    torrents: &mut Vec<domain::Torrent>,
    existing: &[(i32, crate::store::PendingDownload)],
    subscribe_id: domain::SubscribeId,
) {
    let pending: std::collections::HashSet<&str> = existing
        .iter()
        .map(|(_, item)| item.torrent.enclosure.as_str())
        .collect();
    let before = torrents.len();
    torrents.retain(|torrent| !pending.contains(torrent.enclosure.as_str()));
    if before != torrents.len() {
        tracing::debug!(subscribe_id = %subscribe_id, skipped = before - torrents.len(), "跳过已在下载中的 Torrent");
    }
}

fn subscribe_is_active(state: &ApiState, id: domain::SubscribeId) -> Result<bool, String> {
    Ok(state
        .store
        .lock()
        .get_subscribe(id)
        .map_err(|error| error.to_string())?
        .is_some_and(|subscribe| subscribe.tracking_state != "paused"))
}

fn admit_candidates(
    state: &ApiState,
    subscribe: &domain::Subscribe,
    media: &domain::Media,
    filter: &domain::Filter,
    context: &SearchContext,
    torrents: Vec<domain::Torrent>,
    search_keywords: Vec<String>,
) -> Result<Vec<String>, String> {
    let routed =
        crate::delivery::RoutedDownloader::with_pending(state, subscribe, &context.existing);
    let facts = facts_with_pending(context.facts.clone(), &context.existing, subscribe, media);
    let added = admit_and_add(RunInput {
        subscribe,
        media,
        filter,
        wash_filter: context.wash_filter.as_ref(),
        torrents,
        search_keywords,
        facts,
        downloader: &routed,
        library_root: &context.library_root,
        transfer_mode: context.transfer_mode,
        scrape: context.scrape,
        hooks: None,
        naming: Some(&context.naming),
        preserve_removed: false,
    })
    .map_err(|e| {
        tracing::error!(subscribe_id = %subscribe.id, media = %media.title, error = %e, "收录并添加种子失败");
        e.to_string()
    })?;
    notify_search_additions(state, subscribe, media, &added);
    merge_added_pending(state, subscribe, &added, &routed)?;
    record_wanted_round(state, subscribe, &added)?;
    notify_subscription_complete(state, subscribe, media)?;
    Ok(added.add_errors)
}

fn notify_search_additions<D: downloader::Downloader + ?Sized>(
    state: &ApiState,
    subscribe: &domain::Subscribe,
    media: &domain::Media,
    added: &subscribe::Added<'_, D>,
) {
    if !added.chosen.is_empty() {
        tracing::info!(
            subscribe_id = %subscribe.id,
            media = %media.title,
            count = added.chosen.len(),
            "已收录新种子并提交下载"
        );
        notify(
            state,
            "订阅命中新资源",
            &format!(
                "「{}」新增 {} 个候选已提交下载",
                media.title,
                added.chosen.len()
            ),
        );
    } else {
        tracing::debug!(
            subscribe_id = %subscribe.id,
            media = %media.title,
            "本轮没有新收录的种子"
        );
    }
}

fn merge_added_pending<D: downloader::Downloader + ?Sized>(
    state: &ApiState,
    subscribe: &domain::Subscribe,
    added: &subscribe::Added<'_, D>,
    routed: &crate::delivery::RoutedDownloader<'_>,
) -> Result<(), String> {
    let pending: Vec<(i32, crate::store::PendingDownload)> = added
        .chosen
        .iter()
        .map(|scored| {
            (
                scored.score,
                crate::store::PendingDownload {
                    submitted_at: None,
                    torrent: scored.torrent.clone(),
                    release_override: None,
                    downloader_id: routed.used_id(&scored.torrent),
                },
            )
        })
        .collect();
    state
        .store
        .lock()
        .record_pending_submissions(subscribe.id, &pending)
        .map_err(|error| error.to_string())
}

fn facts_with_pending(
    facts: subscribe::SubscribeFacts,
    existing: &[(i32, crate::store::PendingDownload)],
    subscribe: &domain::Subscribe,
    media: &domain::Media,
) -> subscribe::SubscribeFacts {
    let releases = pending_releases(existing);
    subscribe::facts_with_pending(facts, &releases, subscribe, media)
}

pub(crate) fn pending_releases(existing: &[(i32, crate::store::PendingDownload)]) -> Vec<domain::Release> {
    existing
        .iter()
        .map(|(_, pending)| {
            pending
                .release_override
                .clone()
                .unwrap_or_else(|| release::parse(&pending.torrent.title))
        })
        .collect()
}
