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

    // M6: queue replaced files instead of deleting inline; a failed delete
    // must not abort the commit (pending/wanted below) and is retried later.
    for path in &outcome.removed_paths {
        store
            .enqueue_file_deletion(path, "wash_replaced")
            .map_err(|e| e.to_string())?;
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
    store
        .mark_pending_rejected(subscribe.id, &outcome.rejected_enclosures)
        .map_err(|e| e.to_string())?;
    for row in &outcome.ledger {
        store.clear_ledger_missing(&row.path).map_err(|error| {
            tracing::error!(path = %row.path, %error, "清理 Library 缺失标记失败");
            error.to_string()
        })?;
    }
    let library_id = lease.library_id().to_string();
    drop(store);
    crate::http::file_delete::retry_queued_deletions(&state.store);
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

/// D-M3: hand persisted info hashes back to the (possibly fresh) clients so
/// label-less Transmission tasks stay collectable across restarts.
fn restore_identities(
    state: &ApiState,
    subscribe: &Subscribe,
    routed: &crate::delivery::RoutedDownloader<'_>,
    pending: &[(i32, crate::store::PendingDownload)],
) {
    let hashes = match state.store.lock().load_pending_info_hashes(subscribe.id) {
        Ok(hashes) => hashes,
        Err(error) => {
            tracing::warn!(subscribe_id = %subscribe.id, %error, "读取 pending info hash 失败");
            return;
        }
    };
    for (_, item) in pending {
        if let Some(hash) = hashes.get(&item.torrent.enclosure) {
            use downloader::Downloader as _;
            if let Err(error) = routed.remember_identity(&item.torrent, hash) {
                tracing::warn!(torrent = %item.torrent.title, %error, "恢复下载器任务身份失败");
            }
        }
    }
}

fn filter_matched_pending(
    subscribe: &Subscribe,
    media: &Media,
    pending: Vec<(i32, crate::store::PendingDownload)>,
    absolute: Option<&subscribe::AbsoluteEpisodes>,
) -> Vec<ScoredTorrent> {
    pending
        .into_iter()
        .filter_map(|(score, pending)| {
            let release = pending
                .release_override
                .unwrap_or_else(|| release::parse(&pending.torrent.title));
            // G-H2b: absolute episode numbers → catalog season + episode.
            let release = absolute.map_or_else(|| release.clone(), |map| map.apply(&release));
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
            rejected_enclosures: Vec::new(),
            collection_errors: Vec::new(),
            submission_errors: Vec::new(),
            torrents_added: Vec::new(),
        },
        broken: Vec::new(),
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
    crate::http::file_delete::retry_queued_deletions(&state.store);

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
    // H3: release rows whose task provably vanished from the client, so a
    // slot never stays blocked just because nobody opened the task center.
    let pending = crate::pending_reconcile::reconcile_subscribe(state, subscribe.id, pending);
    crate::selective_files::retry_deferred(state, subscribe.id, &pending);

    if pending.is_empty() {
        return Ok(());
    }

    let (media, filter, facts, wash_filter) = load_transfer_context(state, &subscribe)?;
    let routed = crate::delivery::RoutedDownloader::with_pending(state, &subscribe, &pending);
    restore_identities(state, &subscribe, &routed, &pending);
    let absolute = crate::season_check::absolute_map(
        state,
        &media,
        pending.iter().map(|(_, p)| p.torrent.title.as_str()),
    );
    let chosen = filter_matched_pending(&subscribe, &media, pending, absolute.as_ref());

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
            overlap: Default::default(),
            absolute: absolute.as_ref(),
        },
        chosen,
    );

    let destinations = routed.imported_destinations().map_err(|e| e.to_string())?;
    // M7: do not start writing library files for a subscription being deleted.
    if state.subscribe_is_deleting(subscribe_id) {
        tracing::warn!(subscribe_id = %subscribe_id, "订阅已被预留删除，跳过 Transfer");
        return Ok(());
    }
    let known: std::collections::HashSet<std::path::PathBuf> = destinations
        .iter()
        .map(|m| m.destination_path.clone())
        .collect();
    // D-M2: hardlink fallback policy and fallback counting for this round.
    let fallback_copy = state
        .store
        .lock()
        .get_setting(crate::settings_keys::LIBRARY_HARDLINK_FALLBACK)
        .ok()
        .flatten()
        .is_none_or(|value| value.trim() != "fail");
    library::set_hardlink_fallback_copy(fallback_copy);
    let fallbacks_before = library::hardlink_fallback_count();
    let outcome = collect_completed_controlled(
        added,
        &library::Ffprobe::default(),
        &destinations,
        library_lease.control(),
    )
    .map_err(|e| e.to_string())?;
    if state.subscribe_is_deleting(subscribe_id) {
        tracing::warn!(subscribe_id = %subscribe_id, "Transfer 完成但订阅已被预留删除，取消提交并回滚本轮新文件");
        rollback_new_files(&outcome, plan.mode, &known);
        return Ok(());
    }

    if let Err(error) = library_lease.control().check() {
        rollback_new_files(&outcome, plan.mode, &known);
        return Err(error.to_string());
    }
    if plan.scrape {
        attach_metadata_and_chapters(state, &media, &outcome.ledger);
    }

    library_lease.control().check().map_err(|e| e.to_string())?;
    let ledger_count = outcome.ledger.len();
    commit_transfer_outcome(state, &subscribe, &outcome, &library_lease)?;

    crate::http::library::enqueue_probes_for_rows(state, &outcome.ledger);
    let fallbacks = library::hardlink_fallback_count().saturating_sub(fallbacks_before);
    if fallbacks > 0 {
        tracing::warn!(%subscribe_id, fallbacks, "硬链接失败，已回退为复制");
        super::notify(
            state,
            "硬链接回退为复制",
            &format!(
                "「{}」有 {fallbacks} 个文件无法硬链接（下载目录与媒体库可能不在同一设备），已改为复制",
                media.title
            ),
        );
    }
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

/// M7: undo library files this abandoned round created. Only paths that were
/// not already known destinations before the round are touched. Hardlink /
/// Copy: delete the new file (the download keeps its data). Move: move the
/// file back to its download source; if that fails it is left in place.
fn rollback_new_files(
    outcome: &subscribe::RunOutcome,
    mode: Option<library::TransferMode>,
    known: &std::collections::HashSet<std::path::PathBuf>,
) {
    for row in &outcome.ledger {
        let path = std::path::PathBuf::from(&row.path);
        if known.contains(&path) || !path.exists() {
            continue;
        }
        let source = outcome
            .ledger_sources
            .iter()
            .find(|s| s.ledger_path == row.path)
            .map(|s| std::path::PathBuf::from(&s.source_path));
        let result = match (mode, source) {
            (Some(library::TransferMode::Move), Some(source)) => {
                if source.exists() {
                    tracing::warn!(path = %path.display(), "回滚：Move 源路径已存在，保留库文件");
                    continue;
                }
                std::fs::rename(&path, &source)
            }
            (Some(library::TransferMode::Move), None) => {
                tracing::warn!(path = %path.display(), "回滚：Move 模式缺少源路径，保留库文件");
                continue;
            }
            _ => std::fs::remove_file(&path),
        };
        match result {
            Ok(()) => tracing::info!(path = %path.display(), "回滚放弃提交的 Transfer 新文件"),
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "回滚 Transfer 新文件失败")
            }
        }
    }
}
