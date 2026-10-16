//! Pending ↔ Downloader reconciliation shared by the task center and the
//! background Transfer job (H3): a pending row whose task provably vanished
//! from its (reachable) client for longer than the grace period is released.

use std::collections::HashMap;

use domain::{DownloaderId, SubscribeId};

use crate::management::ApiState;

/// Grace period after submission before an absent task counts as gone.
pub(crate) const ABSENT_CLEANUP_GRACE_SECS: i64 = 15 * 60;

/// D-M3: matching that also accepts the persisted info hash, so a task whose
/// ownership tags were stripped (or never existed) is still recognised.
pub(crate) fn snapshot_matches_pending_with_hash(
    torrent: &domain::Torrent,
    stored_hash: Option<&str>,
    snapshot: &downloader::TaskSnapshot,
) -> bool {
    stored_hash.is_some_and(|hash| {
        !snapshot.info_hash.is_empty() && snapshot.info_hash.eq_ignore_ascii_case(hash)
    }) || snapshot_matches_pending(torrent, snapshot)
}

pub(crate) fn snapshot_matches_pending(
    torrent: &domain::Torrent,
    snapshot: &downloader::TaskSnapshot,
) -> bool {
    if let Some(hash) = downloader::magnet_info_hash(&torrent.enclosure) {
        return !snapshot.info_hash.is_empty() && snapshot.info_hash.eq_ignore_ascii_case(&hash);
    }
    // HTTP enclosures are identified only by their per-torrent ownership tag.
    // Real clients report it in `owned_tags` (`tag` keeps the generic marker);
    // `tag` is still checked for legacy / in-memory snapshots.
    let owned = downloader::ownership_tag(&torrent.enclosure);
    snapshot.owned_tags.iter().any(|tag| tag == &owned)
        || snapshot.tag == owned
        || snapshot.tag.split(',').any(|item| item.trim() == owned)
}

/// Whether absence of `torrent` among `snapshots` is evidence it is gone.
/// A magnet is matched by info hash, so a reachable client always proves it.
/// An HTTP enclosure needs ownership tags to be visible on this client: if
/// the client reports tasks but none carries any ownership tag (tags stripped,
/// unsupported labels, …) its absence proves nothing and must not delete rows.
pub(crate) fn absence_is_provable(
    torrent: &domain::Torrent,
    snapshots: &[downloader::TaskSnapshot],
) -> bool {
    // Review M-2 / L-8: an empty list (client just restarted and has not
    // loaded its torrents, wrong instance) is no evidence for any torrent.
    if snapshots.is_empty() {
        return false;
    }
    // A magnet's hash proves absence once the client reports tasks.
    if downloader::magnet_info_hash(&torrent.enclosure).is_some() {
        return true;
    }
    snapshots
        .iter()
        .any(|s| !s.owned_tags.is_empty() || s.tag.starts_with(downloader::OWNED_TAG_PREFIX))
}

/// Whether `item` is provably gone from `snapshots` (client reachable) and
/// past the grace period at `now`.
pub(crate) fn is_absent_and_stale(
    item: &store::PendingDownload,
    snapshots: Option<&[downloader::TaskSnapshot]>,
    now: i64,
) -> bool {
    let Some(snapshots) = snapshots else {
        return false; // client unreachable: absence proves nothing
    };
    let matched = snapshots
        .iter()
        .any(|s| snapshot_matches_pending(&item.torrent, s));
    !matched
        && absence_is_provable(&item.torrent, snapshots)
        && item
            .submitted_at
            .is_some_and(|ts| now - ts >= ABSENT_CLEANUP_GRACE_SECS)
}

/// Review M-1: `is_absent_and_stale` that also honours the persisted info
/// hash (D-M3). Shared by the task center and the background reconcile.
pub(crate) fn is_absent_and_stale_with_hash(
    item: &store::PendingDownload,
    hash: Option<&str>,
    snapshots: Option<&[downloader::TaskSnapshot]>,
    now: i64,
) -> bool {
    let hash_seen = hash.is_some()
        && snapshots.is_some_and(|l| {
            l.iter()
                .any(|s| snapshot_matches_pending_with_hash(&item.torrent, hash, s))
        });
    !hash_seen && is_absent_and_stale(item, snapshots, now)
}

/// Background reconciliation for one Subscribe. The caller must hold the
/// Subscribe guard. Returns the pending rows that remain active.
pub(crate) fn reconcile_subscribe(
    state: &ApiState,
    subscribe_id: SubscribeId,
    pending: Vec<(i32, store::PendingDownload)>,
) -> Vec<(i32, store::PendingDownload)> {
    let mut snapshots: HashMap<Option<DownloaderId>, Option<Vec<downloader::TaskSnapshot>>> =
        HashMap::new();
    for (_, item) in &pending {
        snapshots.entry(item.downloader_id).or_insert_with(|| {
            let client = crate::delivery::client_for_id(state, item.downloader_id).ok()?;
            match client.task_snapshots() {
                Ok(list) => Some(list),
                Err(error) => {
                    tracing::warn!(downloader = ?item.downloader_id, %error, "读取下载器任务快照失败，跳过后台对账");
                    None
                }
            }
        });
    }
    let now = crate::job_loop::unix_now();
    let hashes = state
        .store
        .lock()
        .load_pending_info_hashes(subscribe_id)
        .unwrap_or_default();
    let mut kept = Vec::with_capacity(pending.len());
    for (score, item) in pending {
        let list = snapshots
            .get(&item.downloader_id)
            .and_then(|s| s.as_deref());
        let hash = hashes.get(&item.torrent.enclosure).map(String::as_str);
        if is_absent_and_stale_with_hash(&item, hash, list, now) {
            match state
                .store
                .lock()
                .delete_pending(subscribe_id, &item.torrent.enclosure)
            {
                Ok(()) => {
                    tracing::info!(%subscribe_id, torrent = %item.torrent.title,
                        "后台对账：种子已不在下载器且超过宽限期，释放 pending");
                    continue;
                }
                Err(error) => tracing::error!(%subscribe_id, %error, "后台对账删除 pending 失败"),
            }
        }
        if let Some(list) = list
            && is_stalled(state, subscribe_id, &item, hash, list, now)
        {
            continue;
        }
        kept.push((score, item));
    }
    kept
}

/// H3 stalled: default days without progress before a pending is released.
pub(crate) const DEFAULT_STALLED_TIMEOUT_DAYS: i64 = 7;

fn stalled_timeout_secs(state: &ApiState) -> i64 {
    let days = state
        .store
        .lock()
        .get_setting(crate::settings_keys::SUBSCRIBE_STALLED_TIMEOUT_DAYS)
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|d| *d > 0)
        .unwrap_or(DEFAULT_STALLED_TIMEOUT_DAYS);
    days * 86_400
}

/// Record the task's progress; mark the row `stalled` (task untouched) when
/// it has not progressed for longer than the threshold. Returns true if marked.
fn is_stalled(
    state: &ApiState,
    subscribe_id: SubscribeId,
    item: &store::PendingDownload,
    hash: Option<&str>,
    snapshots: &[downloader::TaskSnapshot],
    now: i64,
) -> bool {
    let Some(snapshot) = snapshots
        .iter()
        .find(|s| snapshot_matches_pending_with_hash(&item.torrent, hash, s))
    else {
        return false;
    };
    if snapshot.progress >= 1.0 {
        return false;
    }
    let enclosure = &item.torrent.enclosure;
    let last = match state.store.lock().touch_pending_progress(
        subscribe_id,
        enclosure,
        snapshot.progress,
        now,
    ) {
        Ok(Some(last)) => last,
        Ok(None) => return false,
        Err(error) => {
            tracing::warn!(%subscribe_id, %error, "记录下载进度失败");
            return false;
        }
    };
    if now - last < stalled_timeout_secs(state) {
        return false;
    }
    match state
        .store
        .lock()
        .mark_pending_stalled(subscribe_id, enclosure)
    {
        Ok(()) => {
            tracing::warn!(%subscribe_id, torrent = %item.torrent.title, idle_secs = now - last,
                "下载长时间无进度：标记 stalled 并释放占位（不删除下载器任务）");
            true
        }
        Err(error) => {
            tracing::error!(%subscribe_id, %error, "标记 stalled 失败");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ABSENT_CLEANUP_GRACE_SECS, is_absent_and_stale};

    fn pending(enclosure: &str, submitted_at: i64) -> store::PendingDownload {
        store::PendingDownload {
            torrent: domain::Torrent {
                site_id: domain::SiteId::new(),
                title: "The.Matrix.1999.2160p".into(),
                enclosure: enclosure.into(),
                size_bytes: None,
                seeders: None,
                free: false,
                hr: false,
                imdb_id: None,
                id: None,
                leechers: None,
                snatched: None,
                upload_time: None,
                detail_url: None,
                category: None,
                poster_url: None,
            },
            release_override: None,
            downloader_id: None,
            submitted_at: Some(submitted_at),
        }
    }

    fn owned_snapshot(enclosure: &str) -> downloader::TaskSnapshot {
        downloader::TaskSnapshot {
            tag: downloader::TASK_TAG.into(),
            owned_tags: vec![downloader::ownership_tag(enclosure)],
            ..Default::default()
        }
    }

    #[test]
    fn absent_needs_reachable_client_proof_and_grace() {
        let now = 10_000;
        let old = now - ABSENT_CLEANUP_GRACE_SECS;
        let fresh = now - 60;
        let mine = "https://pt.example/download.php?id=1";
        let other = [owned_snapshot("https://pt.example/download.php?id=2")];
        assert!(is_absent_and_stale(&pending(mine, old), Some(&other), now));
        assert!(
            !is_absent_and_stale(&pending(mine, fresh), Some(&other), now),
            "grace"
        );
        assert!(
            !is_absent_and_stale(&pending(mine, old), None, now),
            "unreachable"
        );
        assert!(
            !is_absent_and_stale(&pending(mine, old), Some(&[owned_snapshot(mine)]), now),
            "present"
        );
        let generic = [downloader::TaskSnapshot {
            tag: downloader::TASK_TAG.into(),
            ..Default::default()
        }];
        assert!(
            !is_absent_and_stale(&pending(mine, old), Some(&generic), now),
            "unprovable"
        );
    }
}
