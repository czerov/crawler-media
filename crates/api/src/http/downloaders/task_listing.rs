use std::collections::{HashMap, HashSet};

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use domain::DownloaderId;
use serde_json::{Value, json};

use super::tasks::task_state;
use crate::http::ok;
use crate::management::ApiState;
use crate::pending_reconcile::{absence_is_provable, snapshot_matches_pending};

fn prune_orphan_pending(
    state: &ApiState,
) -> (
    Vec<(domain::SubscribeId, i32, store::PendingDownload)>,
    HashSet<Option<DownloaderId>>,
) {
    let store = state.store.lock();
    let rows = store.list_all_pending_routed().unwrap_or_default();
    let mut kept = Vec::with_capacity(rows.len());
    for (subscribe_id, score, item) in rows {
        match store.get_subscribe(subscribe_id) {
            Ok(Some(_)) => kept.push((subscribe_id, score, item)),
            Ok(None) => {
                if let Err(error) = store.delete_pending(subscribe_id, &item.torrent.enclosure) {
                    tracing::error!(%subscribe_id, %error, "清理孤儿 pending 失败");
                } else {
                    tracing::info!(%subscribe_id, torrent = %item.torrent.title, "清理孤儿 pending：订阅已删除");
                }
            }
            Err(error) => {
                tracing::error!(%subscribe_id, %error, "读取订阅失败，保留 pending 行");
                kept.push((subscribe_id, score, item));
            }
        }
    }
    let targets: HashSet<Option<DownloaderId>> =
        kept.iter().map(|(_, _, item)| item.downloader_id).collect();
    (kept, targets)
}

async fn load_task_snapshots(
    state: &ApiState,
    targets: HashSet<Option<DownloaderId>>,
) -> (
    HashMap<Option<DownloaderId>, Vec<downloader::TaskSnapshot>>,
    HashSet<Option<DownloaderId>>,
) {
    let snapshot_state = state.clone();
    tokio::task::spawn_blocking(move || {
        let mut by_dl: HashMap<Option<DownloaderId>, Vec<downloader::TaskSnapshot>> = HashMap::new();
        let mut reachable = HashSet::new();
        for id in targets {
            if let Ok(client) = crate::delivery::client_for_id(&snapshot_state, id) {
                match client.task_snapshots() {
                    Ok(snapshots) => {
                        reachable.insert(id);
                        by_dl.insert(id, snapshots);
                    }
                    Err(error) => {
                        tracing::warn!(downloader = ?id, %error, "读取下载器任务快照失败，无法对齐任务状态");
                    }
                }
            }
        }
        (by_dl, reachable)
    })
    .await
    .unwrap_or_default()
}

fn prune_absent_pending(
    state: &ApiState,
    pending: Vec<(domain::SubscribeId, i32, store::PendingDownload)>,
    snapshots_by_downloader: &HashMap<Option<DownloaderId>, Vec<downloader::TaskSnapshot>>,
    reachable: &HashSet<Option<DownloaderId>>,
) -> Vec<(domain::SubscribeId, i32, store::PendingDownload)> {
    let now = crate::job_loop::unix_now();
    let mut hashes: HashMap<domain::SubscribeId, HashMap<String, String>> = HashMap::new();
    let mut kept = Vec::with_capacity(pending.len());
    for (subscribe_id, score, item) in pending {
        let snapshots = snapshots_by_downloader
            .get(&item.downloader_id)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        // Review M-1: same rule as the background reconcile, including the
        // persisted info hash (D-M3).
        let sub_hashes = hashes.entry(subscribe_id).or_insert_with(|| {
            state
                .store
                .lock()
                .load_pending_info_hashes(subscribe_id)
                .unwrap_or_default()
        });
        let hash = sub_hashes.get(&item.torrent.enclosure).map(String::as_str);
        let reachable_list = reachable.contains(&item.downloader_id).then_some(snapshots);
        let matched = snapshots.iter().any(|s| {
            crate::pending_reconcile::snapshot_matches_pending_with_hash(&item.torrent, hash, s)
        });
        if !matched && reachable_list.is_some() && !absence_is_provable(&item.torrent, snapshots) {
            tracing::error!(%subscribe_id, torrent = %item.torrent.title,
                "下载器未暴露归属标签，无法证明任务已消失，拒绝自动清理 pending");
        }
        let absent = crate::pending_reconcile::is_absent_and_stale_with_hash(
            &item,
            hash,
            reachable_list,
            now,
        );
        if absent && delete_absent_pending(state, subscribe_id, &item) {
            continue;
        }
        kept.push((subscribe_id, score, item));
    }
    kept
}

/// Delete one absent pending row under its Subscribe guard (lock order:
/// guard → store, same as search/Transfer). A busy guard means a search or
/// Transfer is working on this Subscribe right now: skip this round.
fn delete_absent_pending(
    state: &ApiState,
    subscribe_id: domain::SubscribeId,
    item: &store::PendingDownload,
) -> bool {
    let guard = state.subscribe_guard(subscribe_id);
    let Some(_held) = guard.try_lock() else {
        tracing::debug!(%subscribe_id, "订阅正在搜索/转存，本轮跳过缺失任务清理");
        return false;
    };
    let store = state.store.lock();
    // Re-check under the guard: Transfer may have imported it meanwhile.
    let still_active = store
        .load_pending(subscribe_id)
        .map(|rows| {
            rows.iter()
                .any(|(_, p)| p.torrent.enclosure == item.torrent.enclosure)
        })
        .unwrap_or(false);
    if !still_active {
        return true;
    }
    match store.delete_pending(subscribe_id, &item.torrent.enclosure) {
        Ok(()) => {
            tracing::info!(%subscribe_id, torrent = %item.torrent.title, "自动清理：种子已不在下载器且超过宽限期");
            true
        }
        Err(error) => {
            tracing::error!(%subscribe_id, %error, "自动清理缺失任务失败");
            false
        }
    }
}

fn subscribe_media_index(
    store: &store::Store,
    kept: &[(domain::SubscribeId, i32, store::PendingDownload)],
) -> HashMap<String, (String, String, String)> {
    let mut subscribe_media: HashMap<String, (String, String, String)> = HashMap::new();
    for (subscribe_id, _, _) in kept {
        let key = subscribe_id.to_string();
        if subscribe_media.contains_key(&key) {
            continue;
        }
        if let Ok(Some(subscribe)) = store.get_subscribe(*subscribe_id) {
            if let Ok(Some(media)) = store.get_media(subscribe.media_id) {
                subscribe_media.insert(
                    key,
                    (
                        media.title,
                        media.kind.as_str().to_string(),
                        media.id.to_string(),
                    ),
                );
            }
        }
    }
    subscribe_media
}

fn task_row_json(
    store: &store::Store,
    subscribe_id: domain::SubscribeId,
    score: i32,
    pending: &store::PendingDownload,
    snapshots_by_downloader: &HashMap<Option<DownloaderId>, Vec<downloader::TaskSnapshot>>,
    reachable: &HashSet<Option<DownloaderId>>,
    media_info: Option<&(String, String, String)>,
) -> Value {
    let torrent = &pending.torrent;
    let key = subscribe_id.to_string();
    let (media_title, media_kind, media_item_id) = media_info.cloned().unwrap_or_default();
    let subscribed = !media_title.is_empty();
    let snapshot = snapshots_by_downloader
        .get(&pending.downloader_id)
        .and_then(|list| list.iter().find(|s| snapshot_matches_pending(torrent, s)));
    let state = match snapshot {
        Some(s) => task_state(&s.state, s.progress),
        None if reachable.contains(&pending.downloader_id) => "missing",
        None => "queued",
    };
    json!({
        "id": format!("{}:{}", key, torrent.enclosure),
        "info_hash": snapshot.map(|s| s.info_hash.clone()),
        "name": torrent.title,
        "downloader_id": pending.downloader_id.map(|id| id.to_string()),
        "downloader_name": pending.downloader_id.and_then(|id|
            store.get_downloader(id).ok().flatten().map(|row| row.name)),
        "progress": snapshot.map(|s| s.progress),
        "size_bytes": snapshot.map(|s| s.size_bytes).or(torrent.size_bytes),
        "dlspeed_bytes": snapshot.map(|s| s.download_speed),
        "upspeed_bytes": snapshot.map(|s| s.upload_speed),
        "uploaded_bytes": snapshot.map(|s| s.uploaded_bytes),
        "completed_bytes": snapshot.map(|s| s.downloaded_bytes),
        "state": state,
        "error_message": null,
        "site_id": torrent.site_id.to_string(),
        "site_name": null,
        "resolution": null,
        "media_item_id": if subscribed { Some(media_item_id.clone()) } else { None },
        "media_title": if subscribed { Some(media_title.clone()) } else { None },
        "media_kind": if subscribed { Some(media_kind.clone()) } else { None },
        "score": score,
        "subscriptions": if subscribed { vec![json!({
            "id": key,
            "media_item_id": media_item_id,
            "media_title": media_title,
            "media_kind": media_kind,
        })] } else { vec![] },
    })
}

fn map_task_rows(
    state: &ApiState,
    kept: &[(domain::SubscribeId, i32, store::PendingDownload)],
    snapshots_by_downloader: &HashMap<Option<DownloaderId>, Vec<downloader::TaskSnapshot>>,
    reachable: &HashSet<Option<DownloaderId>>,
) -> Vec<Value> {
    let store = state.store.lock();
    let subscribe_media = subscribe_media_index(&store, kept);
    kept.iter()
        .map(|(subscribe_id, score, pending)| {
            let key = subscribe_id.to_string();
            task_row_json(
                &store,
                *subscribe_id,
                *score,
                pending,
                snapshots_by_downloader,
                reachable,
                subscribe_media.get(&key),
            )
        })
        .collect()
}

pub(crate) async fn list_tasks(State(state): State<ApiState>) -> Response {
    let (pending, targets) = prune_orphan_pending(&state);
    let (snapshots_by_downloader, reachable) = load_task_snapshots(&state, targets).await;
    let kept = prune_absent_pending(&state, pending, &snapshots_by_downloader, &reachable);
    let items = map_task_rows(&state, &kept, &snapshots_by_downloader, &reachable);
    ok(json!({ "items": items, "sources": [] })).into_response()
}

#[cfg(test)]
mod tests {
    use crate::pending_reconcile::{absence_is_provable, snapshot_matches_pending};

    fn torrent(enclosure: &str) -> domain::Torrent {
        domain::Torrent {
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
        }
    }

    fn snapshot(owned: &[String]) -> downloader::TaskSnapshot {
        downloader::TaskSnapshot {
            tag: downloader::TASK_TAG.into(),
            owned_tags: owned.to_vec(),
            info_hash: "aaaa".into(),
            ..Default::default()
        }
    }

    #[test]
    fn http_pending_matches_real_client_snapshot_by_owned_tag() {
        let t = torrent("https://pt.example/download.php?id=1");
        let owned = downloader::ownership_tag(&t.enclosure);
        assert!(snapshot_matches_pending(&t, &snapshot(&[owned])));
        let other = downloader::ownership_tag("https://pt.example/download.php?id=2");
        assert!(!snapshot_matches_pending(&t, &snapshot(&[other])));
        assert!(!snapshot_matches_pending(&t, &snapshot(&[])));
    }

    #[test]
    fn absence_needs_visible_ownership_for_http_but_not_magnets() {
        let http = torrent("https://pt.example/download.php?id=1");
        let magnet = torrent("magnet:?xt=urn:btih:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
        let generic_only = vec![snapshot(&[])];
        assert!(!absence_is_provable(&http, &generic_only));
        assert!(absence_is_provable(&magnet, &generic_only));
        // Review M-2: an empty list is no evidence for HTTP torrents.
        assert!(!absence_is_provable(&http, &[]));
        // Review L-8: nor for magnets (client may still be loading torrents).
        assert!(!absence_is_provable(&magnet, &[]));
        let visible = vec![snapshot(&[downloader::ownership_tag("https://x/other")])];
        assert!(absence_is_provable(&http, &visible));
    }
}
