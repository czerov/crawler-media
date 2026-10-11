use std::fs::Metadata;
use std::io;
use std::path::Path;
use std::time::SystemTime;
use domain::LedgerRow;

fn classify_metadata(path: &Path, result: io::Result<Metadata>) -> Option<Metadata> {
    match result {
        Ok(metadata) if metadata.is_file() => Some(metadata),
        Ok(_) => {
            tracing::warn!(path = %path.display(), "Library 文件路径不是普通文件");
            None
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            tracing::debug!(path = %path.display(), "Library 文件暂不可用，跳过排序统计");
            None
        }
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "读取 Library 文件排序统计失败");
            None
        }
    }
}

fn file_metadata(path: &Path) -> Option<Metadata> {
    classify_metadata(path, std::fs::metadata(path))
}

pub(crate) fn total_size(rows: &[LedgerRow]) -> Option<u64> {
    let mut total = 0u64;
    let mut known = false;
    for row in rows {
        if let Some(meta) = file_metadata(Path::new(&row.path)) {
            total += meta.len();
            known = true;
        }
    }
    known.then_some(total)
}

pub(crate) fn added_at(rows: &[LedgerRow]) -> Option<SystemTime> {
    let mut oldest = None;
    for row in rows {
        if let Some(meta) = file_metadata(Path::new(&row.path)) {
            let stamp = meta.created().or_else(|error| {
                tracing::warn!(
                    path = %row.path,
                    %error,
                    "Library added_at uses modified time: creation time unavailable"
                );
                meta.modified()
            });
            match stamp {
                Ok(time) => {
                    oldest = Some(oldest.map_or(time, |curr: SystemTime| curr.min(time)));
                }
                Err(error) => {
                    tracing::warn!(
                        path = %row.path,
                        %error,
                        "无法读取 Library 文件修改时间"
                    );
                }
            }
        }
    }
    oldest
}

#[cfg(test)]
mod tests;
