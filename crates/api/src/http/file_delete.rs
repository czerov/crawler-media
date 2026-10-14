//! Keep a ledger row until its corresponding file is gone.

use crate::Store;

fn remove_physical_file(path: &str) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("删除文件失败 {path}: {error}")),
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
