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

        // 1. Already installed: the marker must name the pinned archive AND the
        //    on-disk binaries must still hash to what was recorded at install.
        if binary_path.is_file() && worker_path.is_file() && hash_path.is_file() {
            let accepted_archive = if policy == InstallPolicy::PreinstalledOnly {
                vec![expected_hash.to_ascii_lowercase(), MANIFEST_MARKER.to_string()]
            } else {
                vec![expected_hash.to_ascii_lowercase()]
            };
            if accepted_archive
                .iter()
                .any(|a| installed_marker_matches(&hash_path, a, &binary_path, &worker_path))
            {
                return Ok(binary_path);
            }
            tracing::warn!(path = %bin_dir.display(), "Obscura install marker or binary hash mismatch; reinstalling");
        }

        fs::create_dir_all(bin_dir)?;

        // 2. Obtain archive depending on policy
        let preinstalled_archive = bin_dir.join("obscura.tar.gz");
        let archive_source_path = match policy {
            InstallPolicy::PreinstalledOnly => {
                if !preinstalled_archive.is_file() {
                    // Image-baked binaries: accept them only with an operator
                    // supplied `obscura.sha256sums` manifest that they match.
                    let manifest = bin_dir.join(PREINSTALLED_MANIFEST);
                    if binary_path.is_file() && worker_path.is_file() && manifest.is_file() {
                        verify_manifest(&manifest, &binary_path, &worker_path)?;
                        let marker = format!(
                            "archive={MANIFEST_MARKER}\nobscura={}\nobscura-worker={}\n",
                            sha256_file(&binary_path)?,
                            sha256_file(&worker_path)?,
                        );
                        fs::write(&hash_path, marker)?;
                        tracing::info!(path = %binary_path.display(), "Preinstalled Obscura binaries verified against manifest");
                        return Ok(binary_path);
                    }
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
            // Full-path match: only top-level `obscura` / `obscura-worker` (an
            // optional leading `./` is tolerated); `x/obscura` is rejected.
            let top_level = path
                .components()
                .filter(|c| !matches!(c, std::path::Component::CurDir))
                .count()
                == 1;

            if !top_level || !EXPECTED_ENTRIES.contains(&file_name) || !entry.header().entry_type().is_file() {
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

        let marker = format!(
            "archive={}\nobscura={}\nobscura-worker={}\n",
            expected_hash.to_ascii_lowercase(),
            sha256_file(&binary_path)?,
            sha256_file(&worker_path)?,
        );
        fs::write(&hash_path, marker)?;
        tracing::info!(path = %binary_path.display(), "Obscura daemon verified and installed");
        Ok(binary_path)
    }
}

/// Manifest for preinstalled binaries (`sha256sum` format: `<hex>  <name>`).
pub const PREINSTALLED_MANIFEST: &str = "obscura.sha256sums";
const MANIFEST_MARKER: &str = "preinstalled-manifest";

fn verify_manifest(manifest: &Path, bin: &Path, worker: &Path) -> Result<(), InstallError> {
    let text = fs::read_to_string(manifest)?;
    for (name, path) in [("obscura", bin), ("obscura-worker", worker)] {
        let expected = text
            .lines()
            .filter_map(|l| {
                let mut it = l.split_whitespace();
                Some((it.next()?, it.next()?.trim_start_matches('*')))
            })
            .find(|(_, n)| *n == name)
            .map(|(h, _)| h.to_ascii_lowercase())
            .ok_or_else(|| InstallError::InvalidArchive(format!("{PREINSTALLED_MANIFEST} has no entry for {name}")))?;
        let actual = sha256_file(path)?;
        if actual != expected {
            return Err(InstallError::HashMismatch { expected, actual });
        }
    }
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String, std::io::Error> {
    let mut reader = BufReader::new(File::open(path)?);
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Marker format: `archive=<sha>\nobscura=<sha>\nobscura-worker=<sha>\n`.
fn installed_marker_matches(marker: &Path, expected_archive: &str, bin: &Path, worker: &Path) -> bool {
    let Ok(text) = fs::read_to_string(marker) else { return false };
    let field = |key: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(key).and_then(|r| r.strip_prefix('=')))
            .map(|v| v.trim().to_ascii_lowercase())
    };
    let (Some(archive), Some(b), Some(w)) = (field("archive"), field("obscura"), field("obscura-worker")) else {
        return false;
    };
    archive.eq_ignore_ascii_case(expected_archive)
        && sha256_file(bin).is_ok_and(|h| h == b)
        && sha256_file(worker).is_ok_and(|h| h == w)
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
            terminate_group(&mut child, self.1);
        }
    }

    fn owns_port(&self, port: u16) -> Option<bool> {
        listener_owned_by_group(port, self.1)
    }
}

/// Linux: find the LISTEN socket inode for 127.0.0.1/::1 `port` in
/// /proc/net/tcp{,6}, then check whether any process in our process group
/// (pgid == daemon pid) holds that inode.
#[cfg(target_os = "linux")]
fn listener_owned_by_group(port: u16, pgid: u32) -> Option<bool> {
    let port_hex = format!("{port:04X}");
    let mut inodes = Vec::new();
    for table in ["/proc/net/tcp", "/proc/net/tcp6"] {
        let Ok(text) = fs::read_to_string(table) else { continue };
        for line in text.lines().skip(1) {
            let cols: Vec<&str> = line.split_whitespace().collect();
            if cols.len() < 10 || cols[3] != "0A" {
                continue;
            }
            if cols[1].rsplit(':').next() == Some(port_hex.as_str()) {
                inodes.push(format!("socket:[{}]", cols[9]));
            }
        }
    }
    if inodes.is_empty() {
        return None;
    }
    let procs = fs::read_dir("/proc").ok()?;
    for entry in procs.flatten() {
        let Some(pid) = entry.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else { continue };
        let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else { continue };
        // pgrp is the 3rd field after the parenthesised comm.
        let Some(after) = stat.rsplit_once(')').map(|(_, r)| r) else { continue };
        let pgrp = after.split_whitespace().nth(2).and_then(|v| v.parse::<u32>().ok());
        if pgrp != Some(pgid) {
            continue;
        }
        let Ok(fds) = fs::read_dir(format!("/proc/{pid}/fd")) else { continue };
        for fd in fds.flatten() {
            if let Ok(link) = fs::read_link(fd.path()) {
                if inodes.iter().any(|i| link.as_os_str() == i.as_str()) {
                    return Some(true);
                }
            }
        }
    }
    Some(false)
}

#[cfg(not(target_os = "linux"))]
fn listener_owned_by_group(_port: u16, _pgid: u32) -> Option<bool> {
    None
}

/// The daemon is spawned as its own process group leader, so signalling the
/// negative pid also reaches `obscura-worker` children. SIGTERM first, then
/// SIGKILL after a short grace period.
fn terminate_group(child: &mut Child, pid: u32) {
    #[cfg(unix)]
    {
        let pgid = -(pid as i32);
        // SAFETY: plain kill(2) on our own process group.
        unsafe { libc::kill(pgid, libc::SIGTERM) };
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            if matches!(child.try_wait(), Ok(Some(_))) {
                // Leader gone; make sure no worker in the group survives.
                unsafe { libc::kill(pgid, libc::SIGKILL) };
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        unsafe { libc::kill(pgid, libc::SIGKILL) };
    }
    #[cfg(not(unix))]
    let _ = pid;
    let _ = child.kill();
    let _ = child.wait();
}

impl Drop for ChildHandle {
    /// Dropping a handle (e.g. a lost start race) must never leave an orphan.
    fn drop(&mut self) {
        if let Some(mut child) = self.0.get_mut().take() {
            terminate_group(&mut child, self.1);
        }
    }
}

/// Env opt-in for `obscura serve --allow-private-network` (default off).
pub const ALLOW_PRIVATE_NETWORK_ENV: &str = "CRAWLER_MEDIA_OBSCURA_ALLOW_PRIVATE_NETWORK";

impl DaemonLauncher for ProcessLauncher {
    fn version(&self, binary: &Path) -> Option<String> {
        let out = Command::new(binary).arg("--version").output().ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    fn spawn(&self, binary: &Path, port: u16) -> Result<Box<dyn DaemonHandle>, String> {
        let mut command = Command::new(binary);
        command
            .arg("serve")
            .arg("--host")
            .arg("127.0.0.1")
            .arg("--port")
            .arg(port.to_string());
        // Obscura blocks loopback/RFC1918 targets by default (SSRF guard).
        // LAN-hosted Sites need an explicit operator opt-in.
        if std::env::var(ALLOW_PRIVATE_NETWORK_ENV).is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true")) {
            command.arg("--allow-private-network");
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // Own process group so stop() can signal obscura-worker too.
            // No PR_SET_PDEATHSIG: it is tied to the spawning *thread*, and
            // the spawn runs on short-lived blocking-pool threads. Crash
            // cleanup relies on the pidfile reaper and shutdown() instead.
            command.process_group(0);
        }
        let child = command
            .spawn()
            .map_err(|e| format!("Failed to spawn Obscura daemon: {e}"))?;
        let pid = child.id();
        Ok(Box::new(ChildHandle(Mutex::new(Some(child)), pid)))
    }
}

#[cfg(all(test, target_os = "linux"))]
mod port_owner_tests {
    use super::listener_owned_by_group;

    #[test]
    fn listener_ownership_is_resolved_by_process_group() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        // SAFETY: getpgrp(2) has no preconditions.
        let own = unsafe { libc::getpgrp() } as u32;
        assert_eq!(listener_owned_by_group(port, own), Some(true));
        assert_eq!(listener_owned_by_group(port, own.wrapping_add(7_777_777)), Some(false));
        drop(listener);
    }
}

#[cfg(all(test, unix))]
mod spawn_thread_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn daemon_survives_the_exit_of_the_thread_that_spawned_it() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("obscura");
        fs::write(&fake, "#!/bin/sh\nexec sleep 30\n").unwrap();
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
        let path = fake.clone();
        let handle = std::thread::spawn(move || ProcessLauncher.spawn(&path, 1).unwrap())
            .join()
            .unwrap();
        std::thread::sleep(Duration::from_millis(300));
        assert!(handle.try_alive(), "spawning thread exit must not kill the daemon");
        handle.kill();
        assert!(!handle.try_alive());
    }
}
