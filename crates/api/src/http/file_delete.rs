//! Keep a ledger row until its corresponding file is gone.

use crate::Store;

fn remove_physical_file(path: &str) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("删除文件失败 {path}: {error}")),
    }
}

/// Review M-3: the path may have been reused by a new import since the
/// deletion was queued (other ledger row, or a file with another size/inode).
fn queued_path_was_reused(
    store: &parking_lot::Mutex<Store>,
    entry: &store::QueuedFileDeletion,
) -> bool {
    let ledger_now = store.lock().ledger_id_at_path(&entry.path).ok().flatten();
    let ledger_changed = entry.ledger_id.is_some() && ledger_now != entry.ledger_id;
    let identity_changed = match (store::file_identity(&entry.path), entry.file_size) {
        (Some((size, inode)), Some(queued_size)) => {
            size != queued_size || matches!((inode, entry.file_inode), (Some(a), Some(b)) if a != b)
        }
        _ => false,
    };
    ledger_changed || identity_changed
}

/// Drop a stale queue row; the file and its ledger row are kept.
fn abandon_queued_deletion(store: &parking_lot::Mutex<Store>, path: &str) {
    tracing::warn!(path = %path, "待删除路径已被新文件复用，放弃删除");
    if let Err(error) = store.lock().abandon_file_deletion(path) {
        tracing::error!(path = %path, %error, "更新待删除文件队列失败");
    }
}

/// M6: retry every queued Library file deletion. Filesystem work happens
/// outside the Store lock; the queue and ledger rows are only cleared once
/// the file is really gone.
pub(crate) fn retry_queued_deletions(store: &parking_lot::Mutex<Store>) {
    let now = crate::job_loop::unix_now();
    // Review M-3: only rows whose backoff expired.
    let queued = match store.lock().list_due_file_deletions(now) {
        Ok(queued) => queued,
        Err(error) => {
            tracing::error!(%error, "读取待删除文件队列失败");
            return;
        }
    };
    for entry in queued {
        let path = entry.path.as_str();
        if queued_path_was_reused(store, &entry) {
            abandon_queued_deletion(store, path);
            continue;
        }
        let result = remove_physical_file(path);
        let store = store.lock();
        let recorded = match &result {
            Ok(()) => store.complete_file_deletion(path),
            Err(error) => {
                tracing::warn!(path = %path, %error, "删除被替换文件失败，保留在待删除队列稍后重试");
                store.record_file_deletion_failure(path, now, error)
            }
        };
        if let Err(error) = recorded {
            tracing::error!(path = %path, %error, "更新待删除文件队列失败");
        }
    }
}

/// Remove a library file, then remove its ledger row. A missing file is an
/// already-completed deletion, while every other filesystem error leaves the
/// ledger intact so callers can report and retry it.
pub(crate) fn remove_file_and_ledger(store: &Store, path: &str) -> Result<(), String> {
    remove_physical_file(path)?;
    store
        .delete_ledger_path(path)
        .map_err(|error| format!("删除记录失败 {path}: {error}"))
}

/// Delete files without holding the global Store mutex, then remove the
/// corresponding ledger rows in one short database critical section.
pub(crate) async fn remove_rows(
    state: &crate::management::ApiState,
    rows: Vec<domain::LedgerRow>,
) -> (usize, Vec<String>) {
    let paths: Vec<String> = rows.into_iter().map(|row| row.path).collect();
    let delete_paths = paths.clone();
    let physical = tokio::task::spawn_blocking(move || {
        delete_paths
            .into_iter()
            .map(|path| {
                let result = remove_physical_file(&path);
                (path, result)
            })
            .collect::<Vec<_>>()
    })
    .await;
    let physical = match physical {
        Ok(results) => results,
        Err(error) => {
            let message = format!("文件清理任务失败: {error}");
            tracing::error!(%error, "媒体库文件清理任务失败");
            return (0, vec![message]);
        }
    };
    let store = state.store.lock();
    let mut removed = 0usize;
    let mut errors = Vec::new();
    for (path, result) in physical {
        if let Err(error) = result {
            tracing::error!(path, %error, "无法删除媒体库文件");
            errors.push(error);
            continue;
        }
        match store.delete_ledger_path(&path) {
            Ok(_) => removed += 1,
            Err(error) => {
                tracing::error!(path, %error, "无法删除媒体库台账记录");
                errors.push(format!("删除记录失败 {path}: {error}"));
            }
        }
    }
    drop(store);
    if removed > 0 {
        // ledger 行已删除：浏览投影的文件事实与归属都变了。
        state.projection.mark_all_dirty();
    }
    (removed, errors)
}

/// Review L-10: `GET /library/pending-deletions` — deletions that stopped
/// retrying (with their last error) and how many are still queued.
pub(crate) async fn pending_deletions(
    axum::extract::State(state): axum::extract::State<crate::management::ApiState>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let store = state.store.lock();
    let queued = store
        .list_pending_file_deletions()
        .map(|q| q.len())
        .unwrap_or(0);
    let stuck: Vec<_> = store
        .list_stuck_file_deletions()
        .unwrap_or_default()
        .into_iter()
        .map(|(path, error)| serde_json::json!({ "path": path, "last_error": error }))
        .collect();
    crate::http::ok(serde_json::json!({ "queued": queued, "stuck": stuck })).into_response()
}
