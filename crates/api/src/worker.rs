use domain::{FetchMode, SubscribeId};
use jobs::{Job, JobKind, Runner};

use crate::management::ApiState;

pub(crate) mod finish_search;
use finish_search::finish_search;
mod search;
pub(crate) use search::{
    report_site_failures, search_torrents_for_subscribe,
};
pub use search::{page_is_full_and_irrelevant, search_keywords_for};
pub(crate) mod transfer;
pub use transfer::transfer_one;

type RssBatch = (Vec<domain::Torrent>, bool, Vec<String>);

pub(crate) fn notify_public(state: &ApiState, title: &str, content: &str) {
    notify(state, title, content);
}

pub(crate) fn notify(state: &ApiState, title: &str, content: &str) {
    // Extract config synchronously (parking_lot guard must not cross await),
    // then send best-effort on the current runtime.
    let (url, token): (Option<String>, Option<String>) = {
        let store = state.store.lock();
        (
            crate::http::notify::notify_url(&store),
            crate::http::notify::notify_token(&store),
        )
    };
    if url.as_deref().unwrap_or_default().is_empty()
        || token.as_deref().unwrap_or_default().is_empty()
    {
        return; // not configured
    }
    let title = title.to_string();
    let content = content.to_string();
    let url = url.clone().unwrap_or_default();
    let token = token.clone().unwrap_or_default();
    // Send on a detached thread so it never blocks the job loop nor the
    // tokio runtime shutdown in tests.
    std::thread::spawn(move || {
        let _ = crate::http::notify::send_configured(&url, &token, &title, &content);
    });
}

fn rss_torrents_for_sites(
    state: &ApiState,
    sites: &[domain::Site],
) -> (Vec<domain::Torrent>, bool, Vec<String>) {
    let outcome = state.indexer.rss(sites);
    let mut failures = Vec::new();
    let succeeded = report_site_failures(&outcome, sites, "rss", &mut failures);
    (outcome.torrents, succeeded, failures)
}

pub struct StateRunner {
    pub state: ApiState,
}

impl Runner for StateRunner {
    fn run(&self, job: &Job) -> Result<(), String> {
        match job.kind {
            JobKind::SubscribeSearch | JobKind::SubscribeRss => search(&self.state, job, None),
            JobKind::Transfer => transfer(&self.state),
            JobKind::WatchIntake => crate::watch_intake::run(&self.state),
            JobKind::Scrape => crate::watch_scrape::run(&self.state),
            JobKind::CheckIn => crate::check_in::run(&self.state),
            JobKind::CatalogRefresh => crate::catalog_refresh::run(&self.state, &job.payload),
            // 维护任务：策略解析、固定 cutoff、批间取消检查与报告都在
            // `maintenance::run` 里。它只在**不持 Queue 锁**的正式 job_loop 路径执行。
            JobKind::DatabaseMaintenance
            | JobKind::JobsRetention
            | JobKind::PlaybackHistoryRetention
            | JobKind::DataRetention => crate::maintenance::run(&self.state, job),
        }
    }
}

fn search(state: &ApiState, job: &Job, rss_batch: Option<&RssBatch>) -> Result<(), String> {
    let subscribe_id = match payload_subscribe_id(&job.payload) {
        Ok(id) => id,
        Err(_) if job.kind == JobKind::SubscribeRss => {
            return run_rss_for_subscriptions(state, job);
        }
        Err(err) => return Err(err),
    };
    let (subscribe, media, filter, sites) = {
        let store = state.store.lock();
        let maybe_sub = store
            .get_subscribe(subscribe_id)
            .map_err(|e| e.to_string())?;
        let Some(subscribe) = maybe_sub else {
            // 订阅已被删除，自动清理残留孤儿 JobDef，避免循环调度与报警。
            drop(store);
            let _ = state.jobs.lock().delete_defs_for_payload(&job.payload);
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
        let sites = store.list_enabled_sites().map_err(|e| e.to_string())?;
        (subscribe, media, filter, sites)
    };
    if subscribe.tracking_state == "paused" {
        return Ok(());
    }
    let mut subscribe = subscribe;
    if crate::completion::skip_search(state, &mut subscribe) {
        return Ok(());
    }
    if job.kind == JobKind::SubscribeSearch
        && subscribe.fetch_mode != FetchMode::Rss
        && crate::air_gate::all_wanted_unaired(state, &subscribe, &media)
    {
        return Ok(());
    }
    let (mut torrents, search_keywords, succeeded, failures) =
        match (job.kind, subscribe.fetch_mode) {
            (JobKind::SubscribeRss, _) | (_, FetchMode::Rss) => {
                let (torrents, succeeded, failures) = rss_batch
                    .cloned()
                    .unwrap_or_else(|| rss_torrents_for_sites(state, &sites));
                (torrents, Vec::new(), succeeded, failures)
            }
            // FetchMode::Both = RSS + 关键词搜索都要：按 enclosure 去重合并，
            // 不能只走关键词搜索（旧行为让「两者」实际只有搜索）。
            (_, FetchMode::Both) => {
                let (mut merged, keywords, search_succeeded, mut failures) =
                    search_torrents_for_subscribe(&state, &sites, &media, &subscribe.coverage);
                let mut seen: std::collections::HashSet<String> =
                    merged.iter().map(|t| t.enclosure.clone()).collect();
                let (rss_torrents, rss_succeeded, rss_failures) =
                    rss_torrents_for_sites(state, &sites);
                failures.extend(rss_failures);
                for torrent in rss_torrents {
                    if seen.insert(torrent.enclosure.clone()) {
                        merged.push(torrent);
                    }
                }
                (
                    merged,
                    keywords,
                    search_succeeded || rss_succeeded,
                    failures,
                )
            }
            _ => {
                let (torrents, keywords, succeeded, failures) =
                    search_torrents_for_subscribe(state, &sites, &media, &subscribe.coverage);
                (torrents, keywords, succeeded, failures)
            }
        };
    if !succeeded {
        return Err(format!("所有索引站点均搜索失败: {}", failures.join("; ")));
    }
    // 换源（replace）Job 会带上要排除的 enclosure：把卡住的那个候选剔除，
    // 让本轮只可能选出**别的**源。普通定时搜索不带该字段，行为不变。
    if let Some(excluded) = payload_exclude_enclosure(&job.payload) {
        let before = torrents.len();
        torrents.retain(|torrent| torrent.enclosure != excluded);
        tracing::info!(
            subscribe_id = %subscribe_id,
            excluded = %excluded,
            dropped = before - torrents.len(),
            "换源搜索已排除卡住的种子"
        );
    }
    let torrents = crate::season_check::drop_misnumbered(state, &subscribe, &media, torrents);
    finish_search(state, &subscribe, torrents, search_keywords)
}

fn run_rss_for_subscriptions(state: &ApiState, job: &Job) -> Result<(), String> {
    let (subscriptions, sites) = {
        let store = state.store.lock();
        (
            store
                .list_all_subscribes()
                .map_err(|error| error.to_string())?,
            store
                .list_enabled_sites()
                .map_err(|error| error.to_string())?,
        )
    };
    let subscriptions: Vec<_> = subscriptions
        .into_iter()
        .filter(|subscribe| {
            subscribe.tracking_state != "paused"
                && matches!(subscribe.fetch_mode, FetchMode::Rss | FetchMode::Both)
        })
        .collect();
    if subscriptions.is_empty() {
        return Ok(());
    }
    let batch = rss_torrents_for_sites(state, &sites);
    if !batch.1 {
        return Err(format!("所有索引站点均搜索失败: {}", batch.2.join("; ")));
    }
    let mut ran = 0usize;
    let mut failed = 0usize;
    for subscribe in subscriptions {
        let mut scoped_job = job.clone();
        scoped_job.payload = serde_json::json!({
            "subscribe_id": subscribe.id.to_string()
        })
        .to_string();
        if let Err(error) = search(state, &scoped_job, Some(&batch)) {
            tracing::error!(subscribe_id = %subscribe.id, %error, "RSS 订阅搜索失败，继续处理其他订阅");
            failed += 1;
        }
        ran += 1;
    }
    tracing::info!(subscriptions = ran, "RSS 订阅轮询完成");
    if ran > 0 && failed == ran {
        return Err(format!("所有 RSS 订阅搜索均失败（{failed} 个订阅）"));
    }
    Ok(())
}

/// Optional `exclude_enclosure` in a search Job payload (replacement searches).
fn payload_exclude_enclosure(payload: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(payload)
        .ok()?
        .get("exclude_enclosure")?
        .as_str()
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// 覆盖范围全部入库 → 幂等发一次「订阅已完成」通知（setting 标记）。
pub(crate) fn notify_subscription_complete(
    state: &ApiState,
    subscribe: &domain::Subscribe,
    media: &domain::Media,
) -> Result<(), String> {
    crate::completion::mark_if_complete(state, subscribe);
    if matches!(
        subscribe.coverage,
        domain::Coverage::Tv {
            episode_to: None,
            ..
        }
    ) {
        return Ok(());
    }
    // L4: a unit only counts while its file is still on disk.
    let done = crate::completion::units_on_disk(state, subscribe);
    let store = state.store.lock();
    let key = format!("subscribe.completed:{}", subscribe.id);
    if !done {
        // L4: no longer complete → the next completion notifies again.
        let _ = store.delete_setting(&key);
        return Ok(());
    }
    if store.get_setting(&key).ok().flatten().is_some() {
        return Ok(()); // 已通知过
    }
    let _ = store.put_setting(&key, "1");
    drop(store);
    notify(
        state,
        "订阅已完成",
        &format!("「{}」已全部入库，订阅目标达成", media.title),
    );
    Ok(())
}

/// 一轮搜索后物化工单历史：仍在追踪的单元累加 search_attempts，
/// chosen 的种子记 grabbed + 种子名，被规则组拒绝的候选记拒绝原因。
fn record_wanted_round<D: downloader::Downloader + ?Sized>(
    state: &ApiState,
    subscribe: &domain::Subscribe,
    added: &subscribe::Added<'_, D>,
) -> Result<(), String> {
    let now = crate::job_loop::unix_now();
    let cap = crate::air_gate::aired_cap(state, subscribe, added.input.media);
    let catalog_max = crate::air_gate::catalog_aired_max(state, subscribe, added.input.media);
    let store = state.store.lock();
    let facts = store
        .load_subscribe_facts(subscribe.id)
        .map_err(|e| e.to_string())?;
    let pending = store
        .load_pending(subscribe.id)
        .map_err(|e| e.to_string())?;
    let history = store
        .load_wanted_history(subscribe.id)
        .map_err(|e| e.to_string())?;
    // 仍在追踪 = coverage 内且未入库的单元。
    let observed = added
        .rejected
        .iter()
        .filter(|candidate| {
            subscribe::candidate_matches_subscribe(subscribe, added.input.media, &candidate.release)
        })
        .filter_map(|candidate| candidate.release.episode_to.or(candidate.release.episode));
    let tracked: Vec<(Option<u32>, Option<u32>)> = crate::air_gate::cap_units(
        crate::open_coverage::units_bounded(
            subscribe,
            added.input.media,
            &facts,
            &pending,
            &history,
            observed,
            Some(catalog_max.unwrap_or(0)),
        ),
        cap,
    )
    .into_iter()
    .filter(|(season, episode)| facts.get(*season, *episode).is_none())
    .collect();
    if !tracked.is_empty() {
        store
            .touch_wanted_searches(subscribe.id, &tracked, now)
            .map_err(|e| e.to_string())?;
    }
    for scored in &added.chosen {
        for (season, episode) in covered_units(&scored.release) {
            store
                .record_wanted_grab(subscribe.id, season, episode, &scored.torrent.title, now)
                .map_err(|e| e.to_string())?;
        }
    }
    let site_names: std::collections::HashMap<domain::SiteId, String> = store
        .list_enabled_sites()
        .unwrap_or_default()
        .into_iter()
        .map(|s| (s.id, s.name))
        .collect();

    for scored in &added.rejected {
        let site_name = site_names
            .get(&scored.torrent.site_id)
            .map(|s| s.as_str())
            .unwrap_or("未知站点");
        let reason = format!("{} · {}：未匹配规则组", site_name, scored.torrent.title);
        for (season, episode) in covered_units(&scored.release) {
            if tracked.contains(&(season, episode)) {
                store
                    .record_wanted_reject(subscribe.id, season, episode, &reason, now)
                    .map_err(|e| e.to_string())?;
            }
        }
    }
    Ok(())
}

/// 一个 Release 覆盖到的单元（season 未知时按 movie 单元记）。
fn covered_units(release: &domain::Release) -> Vec<(Option<u32>, Option<u32>)> {
    if release.season.is_none() {
        return vec![(None, None)];
    }
    let units = release.covered_episodes();
    if units.is_empty() {
        return vec![(release.season, None)];
    }
    units
        .into_iter()
        .map(|(season, episode)| (Some(season), Some(episode)))
        .collect()
}

fn transfer(state: &ApiState) -> Result<(), String> {
    let subscribes = state
        .store
        .lock()
        .list_all_subscribes()
        .map_err(|e| e.to_string())?;
    let mut failures = 0usize;
    for subscribe in subscribes {
        if let Err(error) = transfer_one(state, subscribe.id) {
            tracing::error!(subscribe_id = %subscribe.id, media_id = %subscribe.media_id,
                %error, "Subscribe Transfer 失败，继续处理其他订阅");
            failures += 1;
        }
    }
    if failures == 0 {
        Ok(())
    } else {
        Err(format!("{failures} Subscribe Transfer failed"))
    }
}

fn payload_subscribe_id(payload: &str) -> Result<SubscribeId, String> {
    let value: serde_json::Value = serde_json::from_str(payload).map_err(|err| err.to_string())?;
    let raw = value
        .get("subscribe_id")
        .and_then(|v| v.as_str())
        .ok_or("missing subscribe_id")?;
    raw.parse().map_err(|err: uuid::Error| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::{payload_exclude_enclosure, report_site_failures};

    #[test]
    fn plain_search_payload_has_no_exclusion() {
        assert_eq!(payload_exclude_enclosure(r#"{"subscribe_id":"s-1"}"#), None);
    }

    #[test]
    fn replacement_payload_carries_the_excluded_enclosure() {
        assert_eq!(
            payload_exclude_enclosure(
                r#"{"subscribe_id":"s-1","exclude_enclosure":"https://pt.example/dl?id=1"}"#
            ),
            Some("https://pt.example/dl?id=1".to_string())
        );
    }

    #[test]
    fn huge_release_wanted_units_are_capped() {
        let release = domain::Release {
            title: "Show".into(),
            year: None,
            season: Some(1),
            episode: Some(1),
            episode_to: Some(u32::MAX),
            resolution: None,
            source: None,
            codec: None,
            hdr: None,
            subtitle_language: None,
            audio_language: None,
            group: None,
            confidence: domain::Confidence::High,
            season_inferred: false,
        };
        let units = super::covered_units(&release);
        assert_eq!(units.len(), domain::Coverage::MAX_EPISODES as usize);
        assert_eq!(units[0], (Some(1), Some(1)));
        assert_eq!(
            units[units.len() - 1],
            (Some(1), Some(domain::Coverage::MAX_EPISODES))
        );
    }

    #[test]
    fn empty_or_malformed_exclusion_is_ignored() {
        assert_eq!(
            payload_exclude_enclosure(r#"{"subscribe_id":"s-1","exclude_enclosure":""}"#),
            None
        );
        assert_eq!(payload_exclude_enclosure("not json"), None);
        assert_eq!(payload_exclude_enclosure(r#"{"subscribe_id":"s-1"}"#), None);
    }

    fn site() -> domain::Site {
        domain::Site {
            id: domain::SiteId::new(),
            name: "site".into(),
            url: "https://example.invalid".into(),
            profile_id: "fixture".into(),
            cookie: None,
            api_key: None,
            rss_url: None,
            proxy: None,
            rate_limit_per_minute: None,
            cdp_url: None,
            downloader_id: None,
            enabled: true,
        }
    }

    #[test]
    fn all_site_failures_mark_a_search_round_failed_but_partial_failure_does_not() {
        let sites = [site(), site()];
        let outcome = indexer::SearchOutcome {
            torrents: Vec::new(),
            failures: sites
                .iter()
                .map(|site| indexer::SiteFailure {
                    site_id: site.id,
                    error: "offline".into(),
                })
                .collect(),
        };
        let mut failures = Vec::new();
        assert!(!report_site_failures(
            &outcome,
            &sites,
            "search",
            &mut failures
        ));
        assert_eq!(failures.len(), 2);
        assert_eq!(failures[0], "site: offline");

        let partial = indexer::SearchOutcome {
            torrents: Vec::new(),
            failures: vec![indexer::SiteFailure {
                site_id: sites[0].id,
                error: "offline".into(),
            }],
        };
        assert!(report_site_failures(
            &partial,
            &sites,
            "search",
            &mut Vec::new()
        ));
    }
}
