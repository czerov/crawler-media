use std::path::{Path, PathBuf};

use crate::browse_file_read::BrowseFileRead;

/// `poster.jpg` beside the video file, existence checked through the browse seam.
pub fn poster_beside(path: &str, files: &dyn BrowseFileRead) -> Option<PathBuf> {
    let poster = Path::new(path).parent()?.join("poster.jpg");
    files.is_file(&poster).then_some(poster)
}
