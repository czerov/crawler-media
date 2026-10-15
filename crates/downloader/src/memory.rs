use parking_lot::Mutex;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use domain::Torrent;

use crate::{Downloader, DownloaderError, TaskSnapshot};

pub struct MemoryDownloader {
    stage: PathBuf,
    maps: Mutex<HashMap<String, Vec<PathBuf>>>,
    added: Mutex<Vec<Torrent>>,
    requested: Mutex<Vec<String>>,
    removed: Mutex<Vec<String>>,
    destinations: Mutex<Vec<Option<String>>>,
    snapshots: Mutex<Vec<TaskSnapshot>>,
}

impl MemoryDownloader {
    pub fn new(stage: impl AsRef<Path>) -> Self {
        let stage = stage.as_ref().to_path_buf();
        fs::create_dir_all(&stage).ok();
        Self {
            stage,
            maps: Mutex::new(HashMap::new()),
            added: Mutex::new(Vec::new()),
            requested: Mutex::new(Vec::new()),
            removed: Mutex::new(Vec::new()),
            destinations: Mutex::new(Vec::new()),
            snapshots: Mutex::new(Vec::new()),
        }
    }

    pub fn map_enclosure(&self, enclosure: &str, src: PathBuf) {
        self.map_enclosure_files(enclosure, vec![src]);
    }

    pub fn map_enclosure_files(&self, enclosure: &str, sources: Vec<PathBuf>) {
        self.maps.lock().insert(enclosure.to_string(), sources);
    }

    pub fn added(&self) -> Vec<Torrent> {
        self.added.lock().clone()
    }

    /// 被 remove 过的种子标题（测试断言用）。
    pub fn removed(&self) -> Vec<String> {
        self.removed.lock().clone()
    }

    pub fn destinations(&self) -> Vec<Option<String>> {
        self.destinations.lock().clone()
    }

    pub fn push_snapshot(&self, snapshot: TaskSnapshot) {
        self.snapshots.lock().push(snapshot);
    }

    fn stage_copies(&self, enclosure: &str) -> Result<(), DownloaderError> {
        if !self.requested.lock().iter().any(|item| item == enclosure) {
            return Ok(());
        }
        let maps = self.maps.lock();
        let Some(sources) = maps.get(enclosure) else {
            return Ok(());
        };
        for src in sources.iter().filter(|src| src.exists()) {
            let dest = self
                .stage
                .join(src.file_name().unwrap_or_else(|| "file.bin".as_ref()));
            if !dest.exists() {
                fs::copy(src, dest)?;
            }
        }
        Ok(())
    }
}

fn staged_files(
    stage: &Path,
    maps: &HashMap<String, Vec<PathBuf>>,
    enclosure: &str,
) -> Vec<PathBuf> {
    let Some(sources) = maps.get(enclosure) else {
        return Vec::new();
    };
    sources
        .iter()
        .map(|src| stage.join(src.file_name().unwrap_or_else(|| "file.bin".as_ref())))
        .filter(|dest| dest.exists())
        .collect()
}

impl Downloader for MemoryDownloader {
    fn add(&self, torrent: &Torrent) -> Result<(), DownloaderError> {
        self.added.lock().push(torrent.clone());
        self.requested.lock().push(torrent.enclosure.clone());
        self.stage_copies(&torrent.enclosure)?;
        Ok(())
    }

    fn add_with_options(
        &self,
        torrent: &Torrent,
        download_url: &str,
        save_path: Option<&str>,
    ) -> Result<(), DownloaderError> {
        self.destinations.lock().push(save_path.map(str::to_string));
        let mut resolved = torrent.clone();
        resolved.enclosure = download_url.to_string();
        self.add(&resolved)
    }

    fn remove(&self, torrent: &Torrent, _delete_files: bool) -> Result<(), DownloaderError> {
        self.removed
            .lock()
            .push(format!("legacy:{}", torrent.title));
        Ok(())
    }

    fn owned_identity(&self, torrent: &Torrent) -> Result<Option<String>, DownloaderError> {
        Ok(Some(
            crate::magnet_info_hash(&torrent.enclosure)
                .unwrap_or_else(|| format!("memory:{}", torrent.enclosure)),
        ))
    }

    fn remove_owned(&self, torrent: &Torrent, _delete_files: bool) -> Result<(), DownloaderError> {
        self.removed.lock().push(torrent.title.clone());
        Ok(())
    }

    fn delete_task(&self, info_hash: &str, _delete_files: bool) -> Result<(), DownloaderError> {
        tracing::info!(hash = %info_hash, "从内存下载器删除任务");
        self.removed.lock().push(info_hash.to_string());
        Ok(())
    }

    fn endpoint(&self) -> Option<String> {
        Some("memory|memory".into())
    }

    fn completed_files(&self, torrent: &Torrent) -> Result<Vec<PathBuf>, DownloaderError> {
        self.stage_copies(&torrent.enclosure)?;
        let staged = staged_files(&self.stage, &self.maps.lock(), &torrent.enclosure);
        if !staged.is_empty() {
            return Ok(staged);
        }
        Ok(self
            .maps
            .lock()
            .get(&torrent.enclosure)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|src| src.exists())
            .collect())
    }

    fn task_snapshots(&self) -> Result<Vec<TaskSnapshot>, DownloaderError> {
        let injected = self.snapshots.lock().clone();
        if !injected.is_empty() {
            return Ok(injected);
        }
        Ok(self
            .added
            .lock()
            .iter()
            .map(|torrent| TaskSnapshot {
                tag: crate::ownership_tag(&torrent.enclosure),
                name: torrent.title.clone(),
                progress: 0.0,
                state: "queued".into(),
                size_bytes: torrent.size_bytes.unwrap_or(0),
                downloaded_bytes: 0,
                uploaded_bytes: 0,
                download_speed: 0,
                upload_speed: 0,
                info_hash: String::new(),
                owned_tags: vec![crate::ownership_tag(&torrent.enclosure)],
            })
            .collect())
    }
}
