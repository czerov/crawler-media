use domain::{LedgerRow, Media, Subscribe, SubscribeId};
use filter::ScoredTorrent;
use subscribe::{Added, RunInput, RunOutcome, SubscribeFacts, collect_completed_controlled};

use crate::management::ApiState;
use crate::scrape_store::ScrapeStoreExt;

fn attach_metadata_and_chapters(state: &ApiState, media: &Media, rows: &[LedgerRow]) {
    for row in rows {
        crate::poster_fetch::attach_poster(state, media, std::path::Path::new(&row.path));
        crate::poster_fetch::attach_backdrop(state, media, std::path::Path::new(&row.path));
        crate::http::library_chapters::auto_generate_chapters(
            state,
            std::path::Path::new(&row.path),
        );
    }
}

fn commit_transfer_outcome(
    state: &ApiState,
    subscribe: &Subscribe,
    outcome: &RunOutcome,
    lease: &crate::library_lifecycle::LibraryWorkLease,
) -> Result<(), String> {
    let store = state.store.lock();
    lease.control().check().map_err(|e| e.to_string())?;
    if !store
        .library_accepts_work(lease.library_id())
        .map_err(|e| e.to_string())?
    {
        return Err("目标 Library 正在删除或已不存在".into());
    }
    // T1 / plan §95: 先持久化新 replacement ledger/source mapping 与 facts，
    // 旧文件物理删除绝不早于新 replacement 记录的持久化。
    for row in &outcome.ledger {
        let source_path = outcome
            .ledger_sources
            .iter()
            .find(|source| source.ledger_path == row.path)
            .map(|source| source.source_path.as_str());
        match source_path {
            Some(source_path) => store
                .insert_ledger_with_source(row, std::path::Path::new(source_path))
                .map_err(|e| e.to_string())?,
            None => store.insert_ledger(row).map_err(|e| e.to_string())?,
        }
    }
    store
        .save_subscribe_facts(subscribe.id, &outcome.facts)
        .map_err(|e| e.to_string())?;

    for path in &outcome.removed_paths {
        crate::http::file_delete::remove_file_and_ledger(&store, path)?;
    }
    let now = crate::job_loop::unix_now();
    for row in &outcome.ledger {
        store
            .mark_wanted_imported(subscribe.id, row.season, row.episode, now)
            .map_err(|e| e.to_string())?;
    }
    store
        .mark_pending_imported(subscribe.id, &outcome.transferred_enclosures)
        .map_err(|e| e.to_string())?;
    for row in &outcome.ledger {
        store.clear_ledger_missing(&row.path).map_err(|error| {
            tracing::error!(path = %row.path, %error, "清理 Library 缺失标记失败");
            error.to_string()
        })?;
    }
    let library_id = lease.library_id().to_string();
    drop(store);
    // Transfer 写入新 ledger 行并可能删除旧文件：该 Library 的投影必须重建。
    state.projection.mark_dirty(&library_id);
    Ok(())
}

fn load_transfer_context(
    state: &ApiState,
    subscribe: &Subscribe,
) -> Result<
    (
        Media,
        domain::Filter,
        SubscribeFacts,
        Option<domain::Filter>,
    ),
    String,
> {
    let store = state.store.lock();
    let media = store
        .get_media(subscribe.media_id)
        .map_err(|e| e.to_string())?
        .ok_or("Media not found")?;
    let filter = store
        .get_filter(subscribe.filter_id)
        .map_err(|e| e.to_string())?
        .ok_or("Filter not found")?;
    let facts = store
        .load_library_subscribe_facts(subscribe, media.kind)
        .map_err(|e| e.to_string())?;
    let wash_filter = subscribe
        .wash_cut_filter_id
        .map(|fid| store.get_filter(fid))
        .transpose()
        .map_err(|e| e.to_string())?
        .flatten()
        .or_else(|| store.get_filter(subscribe.filter_id).ok().flatten());
    Ok((media, filter, facts, wash_filter))
}

struct TransferPlan {
    library_id: String,
    root: std::path::PathBuf,
    mode: Option<library::TransferMode>,
    naming: String,
    scrape: bool,
    nfo: bool,
}

fn filter_matched_pending(
    subscribe: &Subscribe,
    media: &Media,
    pending: Vec<(i32, crate::store::PendingDownload)>,
) -> Vec<ScoredTorrent> {
    pending
        .into_iter()
        .filter_map(|(score, pending)| {
            let release = pending
                .release_override
                .unwrap_or_else(|| release::parse(&pending.torrent.title));
            if !subscribe::candidate_matches_subscribe(subscribe, media, &release) {
                tracing::error!(subscribe_id = %subscribe.id, media = %media.title,
                    torrent = %pending.torrent.title, "拒绝转存与订阅不匹配的待处理种子");
                return None;
            }
            Some(ScoredTorrent {
                release,
                torrent: pending.torrent,
                score,
            })
        })
        .collect()
}

fn build_added_input<'a>(
    input: RunInput<'a, crate::delivery::RoutedDownloader<'a>>,
    chosen: Vec<ScoredTorrent>,
) -> Added<'a, crate::delivery::RoutedDownloader<'a>> {
    Added {
        input,
        chosen,
        add_errors: Vec::new(),
        rejected: Vec::new(),
        outcome: subscribe::RunOutcome {
            facts: Default::default(),
            ledger: Vec::new(),
            ledger_sources: Vec::new(),
            removed_paths: Vec::new(),
            completed: false,
            transferred_enclosures: Vec::new(),
            collection_errors: Vec::new(),
            submission_errors: Vec::new(),
            torrents_added: Vec::new(),
        },
    }
}

fn resolve_transfer_plan(
    state: &ApiState,
    media: &Media,
    subscribe: &Subscribe,
) -> Result<TransferPlan, String> {
    let (library_id, library_root, transfer_mode, scrape) = {
        let store = state.store.lock();
        let library_id = match subscribe.library_id {
            Some(id) => id.to_string(),
            None => {
                store
                    .default_library(media.kind)
                    .map_err(|e| e.to_string())?
                    .ok_or("目标 Library 不存在")?
                    .id
            }
        };
        let (root, mode, scrape) =
            crate::directory::transfer_plan_for_library(&store, media.kind, Some(&library_id))
                .map_err(|e| e.to_string())?;
        (library_id, root, mode, scrape)
    };

    let naming = state
        .store
        .lock()
        .naming_pattern(media.kind)
        .map_err(|e| e.to_string())?;
    let nfo = {
        let store = state.store.lock();
        let cfg = store.get_scrape_config().map_err(|e| e.to_string())?;
        scrape && cfg.effective.mirror_nfo
    };
    Ok(TransferPlan {
        library_id,
        root: library_root,
        mode: transfer_mode,
        naming,
        scrape,
        nfo,
    })
}

pub fn transfer_one(state: &ApiState, subscribe_id: SubscribeId) -> Result<(), String> {
    let guard = state.subscribe_guard(subscribe_id);
    let _guard = guard.lock();

    if state.subscribe_is_deleting(subscribe_id) {
        tracing::info!(subscribe_id = %subscribe_id, "订阅正在删除中，跳过 Transfer");
        return Ok(());
    }

    let subscribe = match state.store.lock().get_subscribe(subscribe_id) {
        Ok(Some(s)) => s,
        Ok(None) => {
            tracing::info!(subscribe_id = %subscribe_id, "订阅已不存在，跳过 Transfer");
            return Ok(());
        }
        Err(e) => return Err(e.to_string()),
    };

    let pending = state
        .store
        .lock()
        .load_pending(subscribe.id)
        .map_err(|e| e.to_string())?;

    if pending.is_empty() {
        return Ok(());
    }

    let (media, filter, facts, wash_filter) = load_transfer_context(state, &subscribe)?;
    let routed = crate::delivery::RoutedDownloader::with_pending(state, &subscribe, &pending);
    let chosen = filter_matched_pending(&subscribe, &media, pending);

    let plan = resolve_transfer_plan(state, &media, &subscribe)?;
    let library_lease = state.acquire_library_work(&plan.library_id)?;
    library_lease.control().check().map_err(|e| e.to_string())?;
    tracing::info!(%subscribe_id, pending_count = chosen.len(), "Transfer 开始");

    let added = build_added_input(
        RunInput {
            subscribe: &subscribe,
            media: &media,
            filter: &filter,
            wash_filter: wash_filter.as_ref(),
            torrents: Vec::new(),
            facts,
            search_keywords: Vec::new(),
            downloader: &routed,
            library_root: &plan.root,
            transfer_mode: plan.mode,
            scrape: plan.nfo,
            hooks: None,
            naming: Some(&plan.naming),
            preserve_removed: true,
        },
        chosen,
    );

    let destinations = routed.imported_destinations().map_err(|e| e.to_string())?;
    let outcome = collect_completed_controlled(
        added,
        &library::Ffprobe::default(),
        &destinations,
        library_lease.control(),
    )
    .map_err(|e| e.to_string())?;
    if state.subscribe_is_deleting(subscribe_id) {
        tracing::warn!(subscribe_id = %subscribe_id, "Transfer 完成但订阅已被预留删除，取消提交");
        return Ok(());
    }

    library_lease.control().check().map_err(|e| e.to_string())?;
    if plan.scrape {
        attach_metadata_and_chapters(state, &media, &outcome.ledger);
    }

    library_lease.control().check().map_err(|e| e.to_string())?;
    let ledger_count = outcome.ledger.len();
    commit_transfer_outcome(state, &subscribe, &outcome, &library_lease)?;

    crate::http::library::enqueue_probes_for_rows(state, &outcome.ledger);
    if ledger_count > 0 {
        super::notify(
            state,
            "入库完成",
            &format!("「{}」{} 个文件已整理进媒体库", media.title, ledger_count),
        );
    }
    super::notify_subscription_complete(state, &subscribe, &media)?;
    let transfer_errors: Vec<String> = outcome
        .collection_errors
        .iter()
        .chain(outcome.submission_errors.iter())
        .cloned()
        .collect();
    tracing::info!(%subscribe_id, ledger_count, imported_count = outcome.transferred_enclosures.len(),
        error_count = transfer_errors.len(), "Transfer 完成");
    if !transfer_errors.is_empty() {
        return Err(format!(
            "{} file collection failure(s): {}",
            transfer_errors.len(),
            transfer_errors.join("; ")
        ));
    }
    Ok(())
}
