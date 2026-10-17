use std::str::FromStr;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use domain::{FetchMode, Filter, Media, Site, Subscribe, SubscribeId, UserId};
use serde_json::{Value, json};
use subscribe::RunInput;

use crate::blocking_budget::BlockingError;
use crate::delivery::RoutedDownloader;
use crate::http::subscriptions::ensure_subscribe_owner;
use crate::management::{ApiError, ApiState};
use crate::scrape_store::ScrapeStoreExt;

struct RunContext {
    subscribe: Subscribe,
    media: Media,
    filter: Filter,
    wash_filter: Option<Filter>,
    sites: Vec<Site>,
}

struct RunFactsObservation {
    facts: subscribe::SubscribeFacts,
    saved: subscribe::SubscribeFacts,
    rows: Vec<domain::LedgerRow>,
    qualities: std::collections::HashMap<domain::LedgerId, Result<domain::Release, String>>,
    ownership_libraries: Vec<store::Library>,
}

pub(crate) async fn run_subscribe(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<UserId>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let state_for_work = state.clone();
    // 整段同步业务 pipeline（搜索 → 准入 → 海报）都在 blocking worker 上跑到
    // 结束：订阅 guard / LibraryLease 在闭包内持有，响应语义不变（不是排队返回）。
    let work = state
        .http_work
        .run_to_completion(move || {
            let id = parse_subscribe_id(&id)?;
            run_subscribe_inner(&state_for_work, id, user_id)
        })
        .await;
    match work {
        Ok(result) => result,
        Err(BlockingError::Busy) => Err(ApiError::with(
            StatusCode::SERVICE_UNAVAILABLE,
            "runtime.busy",
            "服务繁忙，请重试".into(),
        )),
        Err(error) => Err(ApiError::internal(error.to_string())),
    }
}

fn run_subscribe_inner(
    state: &ApiState,
    id: SubscribeId,
    user_id: UserId,
) -> Result<Json<Value>, ApiError> {
    let search_context = load_search_context(state, id, user_id)?;
    reject_if_inactive(state, id, &search_context.subscribe)?;
    let (torrents, search_keywords) = fetch_torrents(state, id, &search_context)?;
    let admitted = admit_after_search(state, id, user_id, torrents, search_keywords)?;
    if admitted.scrape {
        for row in &admitted.ledger {
            admitted
                .library_lease
                .control()
                .check()
                .map_err(|e| ApiError::internal(e.to_string()))?;
            crate::poster_fetch::attach_poster(
                state,
                &admitted.media,
                std::path::Path::new(&row.path),
            );
            crate::poster_fetch::attach_backdrop(
                state,
                &admitted.media,
                std::path::Path::new(&row.path),
            );
        }
    }
    if !admitted.run_errors.is_empty() {
        return Err(ApiError::with(
            StatusCode::INTERNAL_SERVER_ERROR,
            "subscribe.run_failed",
            format!(
                "{} subscription processing failure(s): {}",
                admitted.run_errors.len(),
                admitted.run_errors.join("; ")
            ),
        ));
    }
    Ok(Json(json!({
        "ok": true,
        "data": {
            "completed": admitted.completed,
            "ledger_rows": admitted.ledger_rows,
        }
    })))
}

fn parse_subscribe_id(id: &str) -> Result<SubscribeId, ApiError> {
    SubscribeId::from_str(id)
        .map_err(|_| ApiError::invalid("subscribe.invalid", "invalid Subscribe id".into()))
}

fn load_owned_subscribe(
    state: &ApiState,
    id: SubscribeId,
    user_id: UserId,
) -> Result<(Subscribe, Media, Filter, Option<Filter>, Vec<Site>), ApiError> {
    let store = state.store.lock();
    let subscribe = store
        .get_subscribe(id)?
        .ok_or_else(|| ApiError::missing("subscription.missing", "Subscribe not found".into()))?;
    owner_or_missing(&store, &subscribe, user_id)?;
    let media = store
        .get_media(subscribe.media_id)?
        .ok_or_else(|| ApiError::missing("media.missing", "Media not found".into()))?;
    let filter = store
        .get_filter(subscribe.filter_id)?
        .ok_or_else(|| ApiError::missing("filter.missing", "Filter not found".into()))?;
    let wash_filter = subscribe
        .wash_cut_filter_id
        .map(|id| store.get_filter(id))
        .transpose()?
        .flatten();
    let sites = store.list_enabled_sites()?;
    Ok((subscribe, media, filter, wash_filter, sites))
}

fn owner_or_missing(
    store: &crate::Store,
    subscribe: &Subscribe,
    user_id: UserId,
) -> Result<(), ApiError> {
    ensure_subscribe_owner(store, subscribe, user_id)
        .map_err(|_| ApiError::missing("subscription.missing", "Subscribe not found".into()))
}

fn load_search_context(
    state: &ApiState,
    id: SubscribeId,
    user_id: UserId,
) -> Result<RunContext, ApiError> {
    let (subscribe, media, filter, wash_filter, sites) = load_owned_subscribe(state, id, user_id)?;
    Ok(RunContext {
        subscribe,
        media,
        filter,
        wash_filter,
        sites,
    })
}

fn reject_if_inactive(
    state: &ApiState,
    id: SubscribeId,
    subscribe: &Subscribe,
) -> Result<(), ApiError> {
    if state.subscribe_is_deleting(id) {
        return Err(lifecycle_conflict(
            "subscription.deleting",
            "订阅正在删除，无法运行",
        ));
    }
    if subscribe.tracking_state == "paused" {
        return Err(lifecycle_conflict(
            "subscription.paused",
            "订阅已暂停，无法运行",
        ));
    }
    Ok(())
}

fn lifecycle_conflict(code: &'static str, message: &str) -> ApiError {
    ApiError::with(StatusCode::CONFLICT, code, message.into())
}

struct AdmittedRun {
    media: Media,
    scrape: bool,
    completed: bool,
    ledger_rows: usize,
    ledger: Vec<domain::LedgerRow>,
    run_errors: Vec<String>,
    library_lease: crate::library_lifecycle::LibraryWorkLease,
}

fn admit_after_search(
    state: &ApiState,
    id: SubscribeId,
    user_id: UserId,
    torrents: Vec<domain::Torrent>,
    search_keywords: Vec<String>,
) -> Result<AdmittedRun, ApiError> {
    let guard = state.subscribe_guard(id);
    let _guard = guard.lock();
    let context = reload_locked(state, id, user_id)?;
    let routed = RoutedDownloader::new(state, Some(&context.subscribe));
    let (outcome, scrape, library_lease, plan) =
        execute(state, id, &context, &routed, torrents, search_keywords)?;
    let completed = outcome.completed;
    let ledger_rows = outcome.ledger.len();
    let ledger = outcome.ledger.clone();
    let run_errors = outcome
        .submission_errors
        .iter()
        .chain(outcome.collection_errors.iter())
        .cloned()
        .collect::<Vec<_>>();
    persist(state, id, outcome, &routed, &library_lease, &plan)?;
    Ok(AdmittedRun {
        media: context.media,
        scrape,
        completed,
        ledger_rows,
        ledger,
        run_errors,
        library_lease,
    })
}

fn reload_locked(
    state: &ApiState,
    id: SubscribeId,
    user_id: UserId,
) -> Result<RunContext, ApiError> {
    if state.subscribe_is_deleting(id) {
        tracing::info!(subscribe_id = %id, "订阅在搜索期间已删除，跳过下载提交");
        return Err(lifecycle_conflict(
            "subscription.deleting",
            "订阅正在删除，无法运行",
        ));
    }
    let (subscribe, media, filter, wash_filter, sites) = load_owned_subscribe(state, id, user_id)?;
    if subscribe.tracking_state == "paused" {
        tracing::info!(subscribe_id = %id, "订阅在搜索期间已暂停，跳过下载提交");
        return Err(lifecycle_conflict(
            "subscription.paused",
            "订阅已暂停，无法运行",
        ));
    }
    Ok(RunContext {
        subscribe,
        media,
        filter,
        wash_filter,
        sites,
    })
}

fn execute(
    state: &ApiState,
    id: SubscribeId,
    context: &RunContext,
    routed: &RoutedDownloader<'_>,
    mut torrents: Vec<domain::Torrent>,
    search_keywords: Vec<String>,
) -> Result<
    (
        subscribe::RunOutcome,
        bool,
        crate::library_lifecycle::LibraryWorkLease,
        crate::worker::transfer::TransferPlan,
    ),
    ApiError,
> {
    exclude_pending(state, id, &mut torrents)?;
    let facts = load_run_facts(state, id, context)?;
    let plan = crate::worker::transfer::resolve_transfer_plan(state, &context.media, &context.subscribe)
        .map_err(ApiError::internal)?;
    let library_lease = state.acquire_library_work(&plan.library_id).map_err(ApiError::internal)?;
    let destinations = routed.imported_destinations()
        .map_err(|error| ApiError::internal(error.to_string()))?;
    {
        let store = state.store.lock();
        validate_facts_observation(&store, context, &facts)?;
        crate::worker::transfer::validate_transfer_plan(
            &store, &context.subscribe, &library_lease, &plan.revisions, &plan,
        ).map_err(ApiError::internal)?;
    }
    let recorder = crate::worker::transfer::PlannedPublicationRecorder {
        recorder: crate::worker::transfer::PublicationRecorder {
            state, subscribe: &context.subscribe, lease: &library_lease, revisions: plan.revisions.clone(),
        },
        plan: &plan,
    };
    let added = subscribe::admit_and_add(
        RunInput {
            subscribe: &context.subscribe,
            media: &context.media,
            filter: &context.filter,
            wash_filter: context.wash_filter.as_ref(),
            torrents,
            search_keywords,
            facts: facts.facts.clone(),
            downloader: routed,
            library_root: &plan.root,
            transfer_mode: plan.mode,
            scrape: plan.nfo,
            hooks: None,
            naming: Some(&plan.naming),
            preserve_removed: true,
        },
    )?;
    let mut outcome = subscribe::collect_completed_recorded(
        added, &library::Ffprobe::default(), &destinations,
        library_lease.control(), Some(&recorder),
    )?;
    if !outcome.submission_errors.is_empty() && outcome.torrents_added.is_empty() {
        return Err(subscribe::SubscribeError::Downloader(
            downloader::DownloaderError::Message(outcome.submission_errors.join("; ")),
        ).into());
    }
    library_lease.control().check().map_err(|e| ApiError::internal(e.to_string()))?;
    {
        let store = state.store.lock();
        crate::worker::transfer::validate_transfer_plan(
            &store, &context.subscribe, &library_lease, &plan.revisions, &plan,
        ).map_err(ApiError::internal)?;
    }
    outcome.facts.retain_owned();
    outcome.completed = subscribe::is_complete(&context.subscribe, &outcome.facts);
    Ok((outcome, plan.scrape, library_lease, plan))
}

fn load_run_facts(state: &ApiState, id: SubscribeId, context: &RunContext)
    -> Result<RunFactsObservation, ApiError>
{
    let existing = state.store.lock().load_pending(id)?;
    let snapshot = state.store.lock()
        .subscribe_library_snapshot(&context.subscribe, context.media.kind)
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let mut facts = crate::subscribe_library_read::evaluate_subscribe_facts_out_of_lock_with(
        &snapshot, state.browse_files.as_ref(),
    ).map_err(|e| ApiError::internal(e.to_string()))?;
    for (_, pending) in &existing {
        let release = pending
            .release_override
            .clone()
            .unwrap_or_else(|| release::parse(&pending.torrent.title));
        if !subscribe::candidate_matches_subscribe(&context.subscribe, &context.media, &release) {
            continue;
        }
        let units = if release.season.is_none() {
            vec![(None, None)]
        } else {
            let eps = release.covered_episodes();
            if eps.is_empty() {
                vec![(release.season, None)]
            } else {
                eps.into_iter().map(|(s, e)| (Some(s), Some(e))).collect()
            }
        };
        for (season, episode) in units {
            if facts.get(season, episode).is_none() {
                facts.upsert(
                    season,
                    episode,
                    subscribe::QualityFact { score: 0, path: None },
                );
            }
        }
    }
    Ok(RunFactsObservation {
        facts,
        saved: snapshot.saved,
        rows: snapshot.rows,
        qualities: snapshot.qualities,
        ownership_libraries: snapshot.ownership.libraries().to_vec(),
    })
}

fn validate_facts_observation(
    store: &store::Store,
    context: &RunContext,
    observed: &RunFactsObservation,
) -> Result<(), ApiError> {
    let current = store
        .subscribe_library_snapshot(&context.subscribe, context.media.kind)
        .map_err(|e| ApiError::internal(e.to_string()))?;
    if current.saved != observed.saved
        || current.rows != observed.rows
        || current.qualities != observed.qualities
        || current.ownership.libraries() != observed.ownership_libraries
    {
        tracing::warn!(subscribe_id = %context.subscribe.id,
            "Subscribe facts source rows or ownership changed after filesystem observation");
        return Err(ApiError::internal("Subscribe facts source changed during admission".into()));
    }
    Ok(())
}

fn fetch_torrents(
    state: &ApiState,
    id: SubscribeId,
    context: &RunContext,
) -> Result<(Vec<domain::Torrent>, Vec<String>), ApiError> {
    let mut failures = Vec::new();
    let (torrents, keywords, succeeded) = match context.subscribe.fetch_mode {
        FetchMode::Search => {
            let (torrents, keywords, succeeded, search_failures) =
                crate::worker::search_torrents_for_subscribe(
                    state,
                    &context.sites,
                    &context.media,
                    &context.subscribe.coverage,
                );
            failures.extend(search_failures);
            (torrents, keywords, succeeded)
        }
        FetchMode::Rss => {
            let outcome = state.indexer.rss(&context.sites);
            let succeeded = crate::worker::report_site_failures(
                &outcome,
                &context.sites,
                "legacy rss",
                &mut failures,
            );
            (outcome.torrents, Vec::new(), succeeded)
        }
        FetchMode::Both => {
            let (mut merged, keywords, search_succeeded, mut search_failures) =
                crate::worker::search_torrents_for_subscribe(
                    state,
                    &context.sites,
                    &context.media,
                    &context.subscribe.coverage,
                );
            let rss = state.indexer.rss(&context.sites);
            let rss_succeeded = crate::worker::report_site_failures(
                &rss,
                &context.sites,
                "legacy rss",
                &mut search_failures,
            );
            let mut seen: std::collections::HashSet<String> = merged
                .iter()
                .map(|torrent| torrent.enclosure.clone())
                .collect();
            merged.extend(
                rss.torrents
                    .into_iter()
                    .filter(|torrent| seen.insert(torrent.enclosure.clone())),
            );
            failures.extend(search_failures);
            (merged, keywords, search_succeeded || rss_succeeded)
        }
    };
    if succeeded {
        return Ok((torrents, keywords));
    }
    let message = format!("所有索引站点均搜索失败: {}", failures.join("; "));
    tracing::error!(subscribe_id = %id, error = %message, "旧接口订阅搜索失败");
    Err(ApiError::with(
        StatusCode::INTERNAL_SERVER_ERROR,
        "subscribe.run_failed",
        message,
    ))
}

fn exclude_pending(
    state: &ApiState,
    id: SubscribeId,
    torrents: &mut Vec<domain::Torrent>,
) -> Result<(), ApiError> {
    let existing = state.store.lock().load_pending(id)?;
    let enclosures: std::collections::HashSet<&str> = existing
        .iter()
        .map(|(_, pending)| pending.torrent.enclosure.as_str())
        .collect();
    let before = torrents.len();
    torrents.retain(|torrent| !enclosures.contains(torrent.enclosure.as_str()));
    if torrents.len() != before {
        tracing::debug!(subscribe_id = %id, skipped = before - torrents.len(), "跳过已在下载中的 Torrent");
    }
    Ok(())
}

fn persist(
    state: &ApiState,
    id: SubscribeId,
    outcome: subscribe::RunOutcome,
    routed: &RoutedDownloader<'_>,
    lease: &crate::library_lifecycle::LibraryWorkLease,
    plan: &crate::worker::transfer::TransferPlan,
) -> Result<(), ApiError> {
    // Transfer-mode observation is filesystem work: observe it before taking the lock.
    let default_transfer_mode = {
        let store = state.store.lock();
        store.transfer_mode().unwrap_or_else(|_| "copy".into())
    };
    let modes =
        crate::worker::transfer::observe_transfer_modes(&outcome.ledger, &default_transfer_mode);

    let store = state.store.lock();
    lease
        .control()
        .check()
        .map_err(|e| ApiError::internal(e.to_string()))?;
    if !store.library_accepts_work(lease.library_id())? {
        return Err(lifecycle_conflict(
            "library.deleting",
            "目标 Library 正在删除或已不存在",
        ));
    }
    crate::worker::transfer::validate_transfer_plan(
        &store, &store.get_subscribe(id)?.ok_or_else(|| ApiError::internal("Subscribe not found".into()))?,
        lease, &plan.revisions, plan,
    ).map_err(ApiError::internal)?;
    for (row, mode) in outcome.ledger.iter().zip(modes.iter()) {
        let source_path = outcome.ledger_sources.iter()
            .find(|source| source.ledger_path == row.path)
            .map(|source| source.source_path.as_str());
        let existing = store.ledger_by_path(&row.path)?;
        if existing.as_ref().map(|existing| (&existing.id, existing.media_id, &existing.path))
            != Some((&row.id, row.media_id, &row.path))
            || !store.ledger_source_path_matches(&row.path, source_path)?
        {
            return Err(ApiError::internal(format!("Transfer reservation missing or replaced: {}", row.path)));
        }
        store.finalize_transfer(row, mode, source_path)?;
    }
    let now = crate::job_loop::unix_now();
    let transferred = outcome.transferred_enclosures;
    let pending_items: Vec<_> = outcome
        .torrents_added
        .into_iter()
        .map(|(score, torrent)| {
            let downloader_id = routed.used_id(&torrent);
            (
                score,
                crate::store::PendingDownload {
                    torrent,
                    release_override: None,
                    downloader_id,
                    submitted_at: Some(now),
                },
            )
        })
        .collect();
    store.record_pending_submissions(id, &pending_items)?;
    store.mark_pending_imported(id, &transferred)?;
    store.save_subscribe_facts(id, &outcome.facts)?;
    drop(store);

    // Physical deletion of replaced files OUTSIDE the Store lock (after the database
    // commit is durable), then remove their ledger rows in one short critical section.
    let removed_paths = outcome.removed_paths.clone();
    if !removed_paths.is_empty() {
        let mut removed = Vec::with_capacity(removed_paths.len());
        let mut errors = Vec::new();
        for path in &removed_paths {
            match std::fs::remove_file(path) {
                Ok(()) => removed.push(path),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => removed.push(path),
                Err(error) => {
                    tracing::error!(path, %error, "物理删除旧文件失败，保留台账行");
                    errors.push(format!("{path}: {error}"));
                }
            }
        }
        let store = state.store.lock();
        for path in removed {
            if let Err(error) = store.delete_ledger_path(path) {
                tracing::error!(path, %error, "删除已替换文件的台账行失败");
                errors.push(format!("{path}: {error}"));
            }
        }
        if !errors.is_empty() {
            return Err(ApiError::internal(format!("Transfer replacement cleanup failed: {}", errors.join("; "))));
        }
    }

    crate::http::library::enqueue_probes_for_rows(state, &outcome.ledger);
    Ok(())
}
