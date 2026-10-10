use domain::LedgerRow;
use store::Store;

pub(crate) fn ensure_source_available(store: &Store, row: &LedgerRow) -> Result<(), String> {
    let current = store.get_ledger(&row.id.to_string()).map_err(|error| {
        tracing::error!(%error, ledger_id = %row.id, "读取声纹源文件台账失败");
        format!("source_check_failed: {error}")
    })?;
    if current
        .as_ref()
        .is_none_or(|current| current.path != row.path)
    {
        return Err(format!(
            "file_deleted: ledger {} is no longer available",
            row.id
        ));
    }
    if let Some(media) = store.get_media(row.media_id).map_err(|e| e.to_string())? {
        if let Some(owner) = store.library_for_path_strict(std::path::Path::new(&row.path), media.kind)
            .map_err(|e| e.to_string())?
        {
            if !store.library_accepts_work(&owner.id).map_err(|e| e.to_string())? {
                return Err(format!("file_deleted: Library {} is being removed", owner.id));
            }
        }
    }
    match std::path::Path::new(&row.path).try_exists() {
        Ok(true) => Ok(()),
        Ok(false) => Err(format!("file_deleted: {}", row.path)),
        Err(error) => {
            tracing::error!(%error, path = %row.path, "检查声纹源文件失败");
            Err(format!("source_check_failed: {error}"))
        }
    }
}

pub(crate) fn is_file_deleted(error: &str) -> bool {
    error.starts_with("file_deleted")
}
