use domain::Release;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DestinationMapping {
    pub source_path: String,
    pub destination_path: PathBuf,
    pub slots: Vec<(Option<u32>, Option<u32>)>,
    pub quality: Option<Release>,
    pub transfer_pending: bool,
}

impl DestinationMapping {
    pub fn new(source_path: impl Into<String>, destination_path: impl Into<PathBuf>) -> Self {
        Self {
            source_path: source_path.into(),
            destination_path: destination_path.into(),
            slots: Vec::new(),
            quality: None,
            transfer_pending: false,
        }
    }

    pub fn with_slots_and_quality(
        source_path: impl Into<String>,
        destination_path: impl Into<PathBuf>,
        slots: Vec<(Option<u32>, Option<u32>)>,
        quality: Option<Release>,
    ) -> Self {
        Self {
            source_path: source_path.into(),
            destination_path: destination_path.into(),
            slots,
            quality,
            transfer_pending: false,
        }
    }
}
