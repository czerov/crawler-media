use domain::{LedgerRow, Media, Subscribe, SubscribeId};
use filter::ScoredTorrent;
use subscribe::{Added, RunInput, RunOutcome, SubscribeFacts, collect_completed_recorded};

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

pub(crate) fn observe_transfer_modes(rows: &[LedgerRow], default_mode: &str) -> Vec<String> {
    rows.iter()
        .map(|row| {
            let p = std::path::Path::new(&row.path);
            if p.extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("strm"))
            {
                return "strm".to_string();
            }
            let is_hardlink = std::fs::metadata(&row.path)
                .map(|meta| {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::MetadataExt;
                        meta.nlink() > 1
                    }
                    #[cfg(not(unix))]
                    {
                        false
                    }
                })
                .unwrap_or(false);
            if is_hardlink {
                "hardlink".to_string()
            } else {
                default_mode.to_string()
            }
        })
        .collect()
}

fn commit_transfer_outcome(
    state: &ApiState,
    subscribe: &Subscribe,
    outcome: &RunOutcome,
    lease: &crate::library_lifecycle::LibraryWorkLease,
    observation_revisions: &store::ReadRevisions,
    plan: &TransferPlan,
) -> Result<(), String> {
    let default_transfer_mode = {
        let store = state.store.lock();
        store.transfer_mode().unwrap_or_else(|_| "copy".into())
    };

    // Observe transfer modes outside Store lock
    let observed_modes = observe_transfer_modes(&outcome.ledger, &default_transfer_mode);

    // Commit replacement ledger/source mapping and facts in a short Store section
    {
        let store = state.store.lock();
        validate_transfer_plan(&store, subscribe, lease, observation_revisions, plan)?;
        for (row, mode) in outcome.ledger.iter().zip(observed_modes.iter()) {
            let source_path = outcome.ledger_sources.iter()
                .find(|source| source.ledger_path == row.path)
                .map(|source| source.source_path.as_str());
            let existing = store.ledger_by_path(&row.path).map_err(|e| e.to_string())?;
            if existing.as_ref().map(|existing| (&existing.id, existing.media_id, &existing.path))
                != Some((&row.id, row.media_id, &row.path))
                || !store.ledger_source_path_matches(&row.path, source_path).map_err(|e| e.to_string())?
            {
                return Err(format!("Transfer reservation missing or replaced: {}", row.path));
            }
            store.finalize_transfer(row, mode, source_path).map_err(|e| e.to_string())?;
        }
        for path in &outcome.removed_paths {
            if store.ledger_by_path(path).map_err(|e| e.to_string())?
                .is_some_and(|existing| existing.media_id != subscribe.media_id)
            {
                return Err("待替换路径已被其他媒体占用，放弃本轮提交".into());
            }
        }

        store
            .save_subscribe_facts(subscribe.id, &outcome.facts)
            .map_err(|e| e.to_string())?;

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
    }

    cleanup_replaced_files(state, outcome)?;
    let library_id = lease.library_id().to_string();
    state.projection.mark_dirty(&library_id);
    Ok(())
}

fn cleanup_replaced_files(state: &ApiState, outcome: &RunOutcome) -> Result<(), String> {
    // Only retire old rows after their files are gone. The new rows/source
    // mappings remain durable on cleanup failure, so retries reuse them.
    let mut removed = Vec::with_capacity(outcome.removed_paths.len());
    let mut errors = Vec::new();
    for path in &outcome.removed_paths {
        match std::fs::remove_file(path) {
            Ok(()) => removed.push(path),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => removed.push(path),
            Err(error) => {
                tracing::error!(path, %error, "物理删除旧文件失败，保留台账行");
                errors.push(format!("{path}: {error}"));
            }
        }
    }
    if !removed.is_empty() {
        let store = state.store.lock();
        for path in removed {
            if let Err(error) = store.delete_ledger_path(path) {
                tracing::error!(path, %error, "删除已替换文件的台账行失败");
                errors.push(format!("{path}: {error}"));
            }
        }
    }

    if !errors.is_empty() {
        return Err(format!("Transfer replacement cleanup failed: {}", errors.join("; ")));
    }
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
        store::ReadRevisions,
    ),
    String,
> {
    let (media, filter, wash_filter) = {
        let store = state.store.lock();
        let media = store
            .get_media(subscribe.media_id)
            .map_err(|e| e.to_string())?
            .ok_or("Media not found")?;
        let filter = store
            .get_filter(subscribe.filter_id)
            .map_err(|e| e.to_string())?
            .ok_or("Filter not found")?;
        let wash_filter = subscribe
            .wash_cut_filter_id
            .map(|fid| store.get_filter(fid))
            .transpose()
            .map_err(|e| e.to_string())?
            .flatten()
            .or_else(|| store.get_filter(subscribe.filter_id).ok().flatten());
        (media, filter, wash_filter)
    };
    let obs = crate::subscribe_library_read::observe_subscribe_library_facts(
        state,
        subscribe,
        media.kind,
    )?;
    Ok((media, filter, obs.facts, wash_filter, obs.revisions))
}

pub(crate) struct TransferPlan {
    pub(crate) library_id: String,
    library: store::Library,
    pub(crate) revisions: store::ReadRevisions,
    pub(crate) root: std::path::PathBuf,
    pub(crate) mode: Option<library::TransferMode>,
    pub(crate) naming: String,
    pub(crate) scrape: bool,
    pub(crate) nfo: bool,
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

pub(crate) fn resolve_transfer_plan(
    state: &ApiState,
    media: &Media,
    subscribe: &Subscribe,
) -> Result<TransferPlan, String> {
    let store = state.store.lock();
    let library = match subscribe.library_id {
        Some(id) => store.get_library(&id.to_string()),
        None => store.default_library(media.kind),
    }
    .map_err(|e| e.to_string())?
    .ok_or("目标 Library 不存在")?;
    let (root, mode, scrape) =
        crate::directory::transfer_plan_for_library(&store, media.kind, Some(&library.id))
            .map_err(|e| e.to_string())?;
    let naming = store.naming_pattern(media.kind).map_err(|e| e.to_string())?;
    let cfg = store.get_scrape_config().map_err(|e| e.to_string())?;
    let revisions = store.read_revisions().map_err(|e| e.to_string())?;
    Ok(TransferPlan {
        library_id: library.id.clone(),
        library,
        revisions,
        root,
        mode,
        naming,
        scrape,
        nfo: scrape && cfg.effective.mirror_nfo,
    })
}

pub(crate) fn validate_transfer_plan(
    store: &crate::Store,
    subscribe: &Subscribe,
    lease: &crate::library_lifecycle::LibraryWorkLease,
    revisions: &store::ReadRevisions,
    plan: &TransferPlan,
) -> Result<(), String> {
    lease.control().check().map_err(|e| e.to_string())?;
    if !store.library_accepts_work(lease.library_id()).map_err(|e| e.to_string())? {
        return Err("目标 Library 正在删除或已不存在".into());
    }
    let current_revisions = store.read_revisions().map_err(|e| e.to_string())?;
    let library = store.get_library(&plan.library_id).map_err(|e| e.to_string())?;
    let (root, mode, scrape) = crate::directory::transfer_plan_for_library(
        store, plan.library.kind, Some(&plan.library_id),
    ).map_err(|e| e.to_string())?;
    let naming = store.naming_pattern(plan.library.kind).map_err(|e| e.to_string())?;
    let cfg = store.get_scrape_config().map_err(|e| e.to_string())?;
    // Roots and global settings are not all covered by the app revision. Compare
    // the actual plan as well, including changes before the facts snapshot.
    if current_revisions.app != revisions.app
        || current_revisions.app != plan.revisions.app
        || lease.library_id() != plan.library_id
        || library.as_ref() != Some(&plan.library)
        || root != plan.root || mode != plan.mode || scrape != plan.scrape
        || naming != plan.naming || (scrape && cfg.effective.mirror_nfo) != plan.nfo
        || store.get_subscribe(subscribe.id).map_err(|e| e.to_string())?.as_ref() != Some(subscribe)
    {
        tracing::warn!(subscribe_id = %subscribe.id, library_id = %plan.library_id,
            "Transfer source or Library configuration changed");
        return Err("Transfer source or Library configuration changed".into());
    }
    Ok(())
}

pub(crate) struct PublicationRecorder<'a> {
    pub state: &'a ApiState,
    pub subscribe: &'a Subscribe,
    pub lease: &'a crate::library_lifecycle::LibraryWorkLease,
    pub revisions: store::ReadRevisions,
}

impl PublicationRecorder<'_> {
    fn prepare_with_plan(
        &self, row: &LedgerRow, source: &std::path::Path, mode: library::TransferMode,
        plan: Option<&TransferPlan>,
    ) -> Result<LedgerRow, subscribe::SubscribeError> {
        let result = (|| -> Result<LedgerRow, String> {
            let store = self.state.store.lock();
            // This lock spans validation and reservation, never filesystem work.
            if let Some(plan) = plan {
                validate_transfer_plan(&store, self.subscribe, self.lease, &self.revisions, plan)?;
            } else {
                self.lease.control().check().map_err(|error| error.to_string())?;
                if !store.library_accepts_work(self.lease.library_id()).map_err(|e| e.to_string())?
                    || store.read_revisions().map_err(|e| e.to_string())?.app != self.revisions.app
                    || store.get_subscribe(self.subscribe.id).map_err(|e| e.to_string())?.as_ref() != Some(self.subscribe)
                {
                    return Err("Transfer source or Library configuration changed".into());
                }
            }
            let mode = match mode {
                library::TransferMode::Copy => "copy",
                library::TransferMode::Move => "move",
                library::TransferMode::Hardlink => "hardlink",
            };
            store.prepare_transfer(row, mode, &source.display().to_string()).map_err(|e| e.to_string())
        })();
        result.map_err(|error| {
            tracing::error!(path = %row.path, %error, "Transfer publication reservation failed");
            subscribe::SubscribeError::Downloader(downloader::DownloaderError::Message(error))
        })
    }
}

impl subscribe::TransferRecorder for PublicationRecorder<'_> {
    fn prepare(&self, row: &LedgerRow, source: &std::path::Path, mode: library::TransferMode)
        -> Result<LedgerRow, subscribe::SubscribeError>
    {
        self.prepare_with_plan(row, source, mode, None)
    }

    fn published(&self, row: &LedgerRow) -> Result<(), subscribe::SubscribeError> {
        self.state.store.lock().complete_transfer_publication(row).map_err(|error| {
            tracing::error!(path = %row.path, %error, "Transfer publication completion failed");
            subscribe::SubscribeError::Downloader(downloader::DownloaderError::Message(error.to_string()))
        })
    }
}

pub(crate) struct PlannedPublicationRecorder<'a> {
    pub(crate) recorder: PublicationRecorder<'a>,
    pub(crate) plan: &'a TransferPlan,
}

impl subscribe::TransferRecorder for PlannedPublicationRecorder<'_> {
    fn prepare(&self, row: &LedgerRow, source: &std::path::Path, mode: library::TransferMode)
        -> Result<LedgerRow, subscribe::SubscribeError>
    {
        self.recorder.prepare_with_plan(row, source, mode, Some(self.plan))
    }

    fn published(&self, row: &LedgerRow) -> Result<(), subscribe::SubscribeError> {
        self.recorder.published(row)
    }
}

/// Join existing Library work before observing facts or acquiring our own lease.
/// In particular, a first scan must not claim a just-published Transfer file while
/// probing is still running and before its authoritative ledger row is committed.
fn wait_for_transfer_library(state: &ApiState, library_id: &str) -> Result<bool, String> {
    let started = std::time::Instant::now();
    let runtime = tokio::runtime::Handle::try_current().map_err(|error| error.to_string())?;
    // Match the Library lifecycle drain budget; never hold Store while waiting.
    let idle = runtime.block_on(async {
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            state.wait_library_idle(library_id),
        )
        .await
        .is_ok()
    });
    if !idle {
        tracing::warn!(library_id, elapsed_ms = started.elapsed().as_millis(),
            "Library 工作尚未结束，保留待处理下载供下一轮 Transfer 重试");
    }
    Ok(idle)
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

    let media = state.store.lock().get_media(subscribe.media_id)
        .map_err(|error| error.to_string())?.ok_or("Media not found")?;
    let plan = resolve_transfer_plan(state, &media, &subscribe)?;
    if !wait_for_transfer_library(state, &plan.library_id)? {
        return Ok(());
    }
    let (media, filter, facts, wash_filter, observation_revisions) = load_transfer_context(state, &subscribe)?;
    let routed = crate::delivery::RoutedDownloader::with_pending(state, &subscribe, &pending);
    let chosen = filter_matched_pending(&subscribe, &media, pending);

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
    let recorder = PlannedPublicationRecorder {
        recorder: PublicationRecorder { state, subscribe: &subscribe,
            lease: &library_lease, revisions: observation_revisions },
        plan: &plan,
    };
    let outcome = collect_completed_recorded(
        added,
        &library::Ffprobe::default(),
        &destinations,
        library_lease.control(),
        Some(&recorder),
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
    commit_transfer_outcome(state, &subscribe, &outcome, &library_lease, &observation_revisions, &plan)?;

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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    struct EmptyFetcher;

    impl indexer::Fetcher for EmptyFetcher {
        fn fetch(&self, _: &indexer::FetchRequest) -> Result<String, indexer::IndexerError> {
            Err(indexer::IndexerError::Fetch("unexpected request".into()))
        }
    }

    fn fixture() -> (tempfile::TempDir, ApiState, Media, Subscribe) {
        let tmp = tempfile::tempdir().unwrap();
        let state = ApiState::new(
            crate::Store::open(tmp.path().join("data")).unwrap(),
            "test-token".into(), indexer::ProfileSet::load(None).unwrap(),
            Arc::new(EmptyFetcher),
            Arc::new(downloader::MemoryDownloader::new(tmp.path().join("stage"))),
            tmp.path().join("library"),
        ).unwrap();
        let media = Media {
            id: domain::MediaId::new(), kind: domain::MediaKind::Movie,
            title: "The Matrix".into(), year: Some(1999), original_title: None, aliases: Vec::new(),
            tmdb_id: None, douban_id: None, tvdb_id: None, bangumi_id: None, anilist_id: None,
        };
        let filter = domain::Filter {
            id: domain::FilterId::new(), name: "transfer".into(), atoms: Vec::new(),
            keep_old_versions: false,
        };
        let subscribe = Subscribe {
            id: SubscribeId::new(), user_id: domain::UserId::new(), media_id: media.id,
            coverage: domain::Coverage::Movie, fetch_mode: domain::FetchMode::Search,
            filter_id: filter.id, wash_cut: false, keep_old_versions: false,
            wash_cut_filter_id: None, full_season_pack: false, downloader_id: None,
            library_id: None, tracking_state: "active".into(), follow_future: false,
            search_interval_secs: 1800,
        };
        {
            let store = state.store.lock();
            store.insert_media(&media).unwrap();
            store.insert_filter(&filter).unwrap();
            store.insert_subscribe(&subscribe).unwrap();
        }
        (tmp, state, media, subscribe)
    }

    fn candidate(media: &Media, plan: &TransferPlan) -> LedgerRow {
        LedgerRow {
            id: domain::LedgerId::new(), media_id: media.id,
            path: plan.root.join("The Matrix.mkv").display().to_string(),
            season: None, episode: None, resolution: None, codec: None, hdr: None,
            quality_source: domain::QualitySource::Release,
            confidence: domain::Confidence::High, filter_score: Some(100),
        }
    }

    #[test]
    fn changed_root_is_rejected_before_reservation_even_with_a_new_facts_snapshot() {
        let (tmp, state, media, subscribe) = fixture();
        let plan = resolve_transfer_plan(&state, &media, &subscribe).unwrap();
        let lease = state.acquire_library_work(&plan.library_id).unwrap();
        state.store.lock().set_library_root(media.kind, tmp.path().join("new-root").to_str().unwrap()).unwrap();
        let revisions = state.store.lock().read_revisions().unwrap();
        assert_eq!(revisions.app, plan.revisions.app, "root writes evade the app guard");
        let recorder = PlannedPublicationRecorder {
            recorder: PublicationRecorder { state: &state, subscribe: &subscribe, lease: &lease, revisions },
            plan: &plan,
        };
        let row = candidate(&media, &plan);
        let result = subscribe::TransferRecorder::prepare(
            &recorder, &row, &tmp.path().join("source.mkv"), library::TransferMode::Copy,
        );
        assert!(result.is_err(), "obsolete root must not be reserved: {result:?}");
        assert!(state.store.lock().list_ledger().unwrap().is_empty());
        assert!(!std::path::Path::new(&row.path).exists());
    }

    #[test]
    fn changed_transfer_setting_is_rejected_before_reservation() {
        let (tmp, state, media, subscribe) = fixture();
        state.store.lock().set_transfer_mode("copy").unwrap();
        let plan = resolve_transfer_plan(&state, &media, &subscribe).unwrap();
        let lease = state.acquire_library_work(&plan.library_id).unwrap();
        state.store.lock().set_transfer_mode("move").unwrap();
        let revisions = state.store.lock().read_revisions().unwrap();
        assert_eq!(revisions.app, plan.revisions.app, "settings evade the app guard");
        let recorder = PlannedPublicationRecorder {
            recorder: PublicationRecorder { state: &state, subscribe: &subscribe, lease: &lease, revisions },
            plan: &plan,
        };
        let row = candidate(&media, &plan);
        let result = subscribe::TransferRecorder::prepare(
            &recorder, &row, &tmp.path().join("source.mkv"), library::TransferMode::Copy,
        );
        assert!(result.is_err(), "obsolete mode must not be reserved: {result:?}");
        assert!(state.store.lock().list_ledger().unwrap().is_empty());
        assert!(!std::path::Path::new(&row.path).exists());
    }
}
