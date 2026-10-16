use std::fs::{self, File};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::Duration;

use flate2::read::GzDecoder;
use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use tar::Archive;

use crate::obscura_manager::{DaemonHandle, DaemonLauncher, InstallPolicy};

pub const OBSCURA_VERSION: &str = "v0.2.3";
pub const MAX_ARCHIVE_BYTES: u64 = 200 * 1024 * 1024;
pub const OBSCURA_ASSETS: &[(&str, &str, &str, &str, u64)] = &[
    (
        "linux",
        "x86_64",
        "obscura-x86_64-linux.tar.gz",
        "1534d1e6ddaf3d080ec4091eb41d0a4d8cc042a48b607d3c410fc13b482a9eec",
        70_880_637,
    ),
    (
        "linux",
        "aarch64",
        "obscura-aarch64-linux.tar.gz",
        "5ecf980bca3060236a7a86ec7ed83d943e6598ee87caa46d20325d90bc75f979",
        68_921_993,
    ),
    (
        "macos",
        "x86_64",
        "obscura-x86_64-macos.tar.gz",
        "d7c48122debc2ad9b24842df44560860dba765ea928b3f636b7f053225245116",
        68_697_908,
    ),
    (
        "macos",
        "aarch64",
        "obscura-aarch64-macos.tar.gz",
        "45653cfad226f1c9b415603a2ed59477fcbd6335c742338ce133c05de0bdd056",
        66_569_973,
    ),
];
pub const EXPECTED_ENTRIES: &[&str] = &["obscura", "obscura-worker"];

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("binary is not preinstalled")]
    NotInstalled,
    #[error("hash mismatch: expected {expected}, actual {actual}")]
    HashMismatch { expected: String, actual: String },
    #[error("unsupported platform: os={os}, arch={arch}")]
    Unsupported { os: String, arch: String },
    #[error("download failed: {0}")]
    Download(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid archive: {0}")]
    InvalidArchive(String),
}

pub struct ReleaseInstaller {
    base_url: String,
    custom_assets: Option<&'static [(&'static str, &'static str, &'static str, &'static str, u64)]>,
    max_archive_bytes: u64,
}

impl Default for ReleaseInstaller {
    fn default() -> Self {
        Self::new()
    }
}

impl ReleaseInstaller {
    pub fn new() -> Self {
        Self {
            base_url: format!("https://github.com/h4ckf0r0day/obscura/releases/download/{OBSCURA_VERSION}"),
            custom_assets: None,
            max_archive_bytes: MAX_ARCHIVE_BYTES,
        }
    }

    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    pub fn with_assets(
        mut self,
        assets: &'static [(&'static str, &'static str, &'static str, &'static str, u64)],
    ) -> Self {
        self.custom_assets = Some(assets);
        self
    }

    pub fn with_max_bytes(mut self, bytes: u64) -> Self {
        self.max_archive_bytes = bytes;
        self
    }
}

impl crate::obscura_manager::ObscuraInstaller for ReleaseInstaller {
    fn ensure_installed(&self, bin_dir: &Path, policy: InstallPolicy) -> Result<PathBuf, InstallError> {
        let binary_path = bin_dir.join(if cfg!(windows) { "obscura.exe" } else { "obscura" });
        let worker_path = bin_dir.join(if cfg!(windows) { "obscura-worker.exe" } else { "obscura-worker" });
        let hash_path = bin_dir.join(".obscura.sha256");

        let os = std::env::consts::OS;
        let arch = std::env::consts::ARCH;
        let assets = self.custom_assets.unwrap_or(OBSCURA_ASSETS);
        let (_, _, asset_name, expected_hash, expected_size) = assets
            .iter()
            .find(|(o, a, _, _, _)| *o == os && *a == arch)
            .copied()
            .ok_or_else(|| InstallError::Unsupported {
                os: os.into(),
                arch: arch.into(),
            })?;

        // 1. Check if already installed and verified
        if binary_path.is_file() && worker_path.is_file() && hash_path.is_file() {
            if let Ok(recorded) = fs::read_to_string(&hash_path) {
                if recorded.trim().eq_ignore_ascii_case(expected_hash) {
                    return Ok(binary_path);
                }
            }
        }

        fs::create_dir_all(bin_dir)?;

        // 2. Obtain archive depending on policy
        let preinstalled_archive = bin_dir.join("obscura.tar.gz");
        let archive_source_path = match policy {
            InstallPolicy::PreinstalledOnly => {
                if !preinstalled_archive.is_file() {
                    return Err(InstallError::NotInstalled);
                }
                preinstalled_archive.clone()
            }
            InstallPolicy::Download => {
                let download_url = format!("{}/{}", self.base_url, asset_name);
                tracing::info!(url = %download_url, "Downloading Obscura browser daemon");

                let agent = ureq::Agent::config_builder()
                    .timeout_connect(Some(Duration::from_secs(10)))
                    .timeout_global(Some(Duration::from_secs(300)))
                    .build()
                    .new_agent();

                let response = agent
                    .get(&download_url)
                    .call()
                    .map_err(|e| InstallError::Download(e.to_string()))?;

                if let Some(len_header) = response.headers().get("content-length") {
                    if let Ok(len_str) = len_header.to_str() {
                        if let Ok(content_len) = len_str.parse::<u64>() {
                            if content_len > self.max_archive_bytes || content_len != expected_size {
                                return Err(InstallError::InvalidArchive(format!(
                                    "Content-Length mismatch: expected {expected_size}, got {content_len}"
                                )));
                            }
                        }
                    }
                }

                let nonce = uuid::Uuid::new_v4().simple().to_string();
                let part_path = bin_dir.join(format!(".obscura-{nonce}.tar.gz.part"));
                let mut reader = response.into_body().into_reader();
                let mut part_file = File::create(&part_path)?;

                let mut hasher = Sha256::new();
                let mut buf = [0u8; 8192];
                let mut total_read = 0u64;

                loop {
                    let n = reader.read(&mut buf)?;
                    if n == 0 {
                        break;
                    }
                    total_read += n as u64;
                    if total_read > self.max_archive_bytes {
                        let _ = fs::remove_file(&part_path);
                        return Err(InstallError::InvalidArchive(
                            "Archive exceeds maximum allowed size".into(),
                        ));
                    }
                    hasher.update(&buf[..n]);
                    part_file.write_all(&buf[..n])?;
                }
                part_file.flush()?;
                drop(part_file);

                let actual_hash = format!("{:x}", hasher.finalize());
                if !actual_hash.eq_ignore_ascii_case(expected_hash) {
                    let _ = fs::remove_file(&part_path);
                    return Err(InstallError::HashMismatch {
                        expected: expected_hash.to_string(),
                        actual: actual_hash,
                    });
                }
                part_path
            }
        };

        // 3. Verify and unpack archive
        let file = File::open(&archive_source_path).map_err(|e| InstallError::Io(std::io::Error::new(e.kind(), format!("open archive_source_path: {e}"))))?;
        let mut hasher = Sha256::new();
        let mut buf_reader = BufReader::new(file);
        let mut buf = [0u8; 8192];
        loop {
            let n = buf_reader.read(&mut buf).map_err(|e| InstallError::Io(std::io::Error::new(e.kind(), format!("buf_reader.read: {e}"))))?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        let actual_hash = format!("{:x}", hasher.finalize());
        if !actual_hash.eq_ignore_ascii_case(expected_hash) {
            if archive_source_path != preinstalled_archive {
                let _ = fs::remove_file(&archive_source_path);
            }
            return Err(InstallError::HashMismatch {
                expected: expected_hash.to_string(),
                actual: actual_hash,
            });
        }

        // Unpack into temporary staging directory
        let nonce = uuid::Uuid::new_v4().simple().to_string();
        let temp_unpack_dir = bin_dir.join(format!(".obscura-unpack-{nonce}"));
        fs::create_dir_all(&temp_unpack_dir).map_err(|e| InstallError::Io(std::io::Error::new(e.kind(), format!("create temp_unpack_dir: {e}"))))?;

        let tar_gz = File::open(&archive_source_path).map_err(|e| InstallError::Io(std::io::Error::new(e.kind(), format!("open tar_gz: {e}"))))?;
        let tar = GzDecoder::new(BufReader::new(tar_gz));
        let mut archive = Archive::new(tar);

        let entries = archive.entries().map_err(|e| InstallError::Io(std::io::Error::new(e.kind(), format!("archive.entries: {e}"))))?;
        for entry in entries {
            let mut entry = entry.map_err(|e| InstallError::Io(std::io::Error::new(e.kind(), format!("entry item: {e}"))))?;
            let path = entry.path().map_err(|e| InstallError::Io(std::io::Error::new(e.kind(), format!("entry.path: {e}"))))?.to_path_buf();
            let file_name = path
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| InstallError::InvalidArchive("invalid entry filename".into()))?;

            if !EXPECTED_ENTRIES.contains(&file_name) || !entry.header().entry_type().is_file() {
                let _ = fs::remove_dir_all(&temp_unpack_dir);
                if archive_source_path != preinstalled_archive {
                    let _ = fs::remove_file(&archive_source_path);
                }
                return Err(InstallError::InvalidArchive(format!(
                    "unexpected or non-regular archive entry: {file_name}"
                )));
            }
            entry
                .unpack_in(&temp_unpack_dir)
                .map_err(|e| InstallError::InvalidArchive(format!("unpack_in '{file_name}': {e}")))?;
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for entry_name in EXPECTED_ENTRIES {
                let staged = temp_unpack_dir.join(entry_name);
                if staged.exists() {
                    let _ = fs::set_permissions(&staged, fs::Permissions::from_mode(0o755));
                }
            }
        }

        // Move to final destination
        for entry_name in EXPECTED_ENTRIES {
            let staged = temp_unpack_dir.join(entry_name);
            let target = bin_dir.join(entry_name);
            fs::rename(&staged, &target).map_err(|e| {
                InstallError::Io(std::io::Error::new(
                    e.kind(),
                    format!("rename {:?} to {:?}: {e}", staged, target),
                ))
            })?;
        }
        let _ = fs::remove_dir_all(&temp_unpack_dir);

        if archive_source_path != preinstalled_archive {
            let _ = fs::remove_file(&archive_source_path);
        }

        fs::write(&hash_path, expected_hash)?;
        tracing::info!(path = %binary_path.display(), "Obscura daemon verified and installed");
        Ok(binary_path)
    }
}

pub struct ProcessLauncher;

pub struct ChildHandle(Mutex<Option<Child>>, u32);

impl DaemonHandle for ChildHandle {
    fn pid(&self) -> u32 {
        self.1
    }

    fn try_alive(&self) -> bool {
        let mut guard = self.0.lock();
        if let Some(child) = guard.as_mut() {
            matches!(child.try_wait(), Ok(None))
        } else {
            false
        }
    }

    fn kill(&self) {
        let mut guard = self.0.lock();
        if let Some(mut child) = guard.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl DaemonLauncher for ProcessLauncher {
    fn spawn(&self, binary: &Path, port: u16) -> Result<Box<dyn DaemonHandle>, String> {
        let child = Command::new(binary)
            .arg("serve")
            .arg("--host")
            .arg("127.0.0.1")
            .arg("--port")
            .arg(port.to_string())
            .spawn()
            .map_err(|e| format!("Failed to spawn Obscura daemon: {e}"))?;
        let pid = child.id();
        Ok(Box::new(ChildHandle(Mutex::new(Some(child)), pid)))
    }
}
