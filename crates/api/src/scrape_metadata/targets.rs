use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use domain::LedgerRow;

pub(crate) fn video_present(path: &Path) -> bool {
    match std::fs::metadata(path) {
        Ok(meta) => meta.is_file(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            tracing::debug!(path = %path.display(), "媒体文件已缺失，跳过附属文件处理");
            false
        }
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "无法读取媒体文件状态");
            false
        }
    }
}

pub(crate) fn live_groups(rows: &[LedgerRow]) -> BTreeMap<PathBuf, Vec<LedgerRow>> {
    let mut groups = BTreeMap::new();
    for row in rows {
        let path = Path::new(&row.path);
        if video_present(path) {
            groups
                .entry(super::show_root(path, row))
                .or_insert_with(Vec::new)
                .push(row.clone());
        }
    }
    groups
}
