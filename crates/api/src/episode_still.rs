use std::path::{Path, PathBuf};

/// A still belongs to one episode file, including when a season is stored flat.
pub(crate) fn path(video: &Path) -> PathBuf {
    let stem = video
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default();
    video.with_file_name(format!("{stem}-thumb.jpg"))
}

/// Older libraries may have a shared `still.jpg` or Emby style `<stem>.jpg` / `<stem>-still.jpg`.
pub(crate) fn existing(video: &Path) -> Option<PathBuf> {
    existing_with(video, &crate::browse_file_read::LocalBrowseFileRead)
}

/// Same candidate order as [`existing`], but the existence check goes through the
/// injected browse file seam so the projection builder can count / gate its IO.
pub(crate) fn existing_with(
    video: &Path,
    files: &dyn crate::browse_file_read::BrowseFileRead,
) -> Option<PathBuf> {
    let stem = video
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default();
    let parent = video.parent()?;
    for candidate in [
        path(video),
        parent.join(format!("{stem}-still.jpg")),
        parent.join(format!("{stem}.jpg")),
        parent.join("still.jpg"),
    ] {
        if files.is_file(&candidate) {
            return Some(candidate);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{existing, path};

    #[test]
    fn path_uses_the_jellyfin_thumb_suffix() {
        let video = std::path::Path::new("/library/show/S01E01.strm");
        assert_eq!(
            path(video),
            std::path::PathBuf::from("/library/show/S01E01-thumb.jpg")
        );
    }

    #[test]
    fn existing_reads_an_emby_jpeg_already_on_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let video = tmp.path().join("S01E01.strm");
        std::fs::write(&video, b"https://cdn.example/a.mkv").unwrap();
        let bare = tmp.path().join("S01E01.jpg");
        std::fs::write(&bare, b"jpeg").unwrap();
        assert_eq!(existing(&video), Some(bare));
    }
}
