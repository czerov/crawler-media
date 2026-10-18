use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use api::obscura_manager::{InstallPolicy, ObscuraInstaller, ReleaseInstaller};
use flate2::write::GzEncoder;
use sha2::{Digest, Sha256};
use tar::Builder;
use tempfile::tempdir;

fn make_tar_gz(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), flate2::Compression::default());
    {
        let mut tar = Builder::new(&mut encoder);
        for &(name, data) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o755);
            header.set_entry_type(tar::EntryType::Regular);
            tar.append_data(&mut header, name, data).unwrap();
        }
        tar.finish().unwrap();
    }
    encoder.finish().unwrap()
}

fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

#[test]
fn preinstalled_only_never_touches_the_network() {
    let tmp = tempdir().unwrap();
    let bin_dir = tmp.path().join("bin");

    let installer = ReleaseInstaller::new().with_base_url("http://127.0.0.1:1"); // unreachable
    let res = installer.ensure_installed(&bin_dir, InstallPolicy::PreinstalledOnly);
    assert!(res.is_err());
    assert!(!bin_dir.join("obscura").exists());
}

#[test]
fn preinstalled_binary_without_matching_hash_is_not_installed() {
    let tmp = tempdir().unwrap();
    let bin_dir = tmp.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();

    let invalid_archive = make_tar_gz(&[("obscura", b"fake"), ("obscura-worker", b"worker")]);
    fs::write(bin_dir.join("obscura.tar.gz"), invalid_archive).unwrap();

    let installer = ReleaseInstaller::new();
    let res = installer.ensure_installed(&bin_dir, InstallPolicy::PreinstalledOnly);
    assert!(res.is_err());
    assert!(!bin_dir.join("obscura").exists());
}

#[test]
fn verified_archive_is_installed_atomically() {
    let tmp = tempdir().unwrap();
    let bin_dir = tmp.path().join("bin");

    let archive = make_tar_gz(&[("obscura", b"binary"), ("obscura-worker", b"worker")]);
    let hash = sha256_hex(&archive);
    let size = archive.len() as u64;

    let hash_leaked = Box::leak(hash.into_boxed_str()) as &'static str;
    let custom_asset: &'static [(&'static str, &'static str, &'static str, &'static str, u64)] = Box::leak(Box::new([
        (std::env::consts::OS, std::env::consts::ARCH, "asset.tar.gz", hash_leaked, size),
    ]));

    // Serve archive over local HTTP server
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_c = stop.clone();
    let archive_c = archive.clone();
    std::thread::spawn(move || {
        listener.set_nonblocking(true).unwrap();
        while !stop_c.load(Ordering::Relaxed) {
            if let Ok((mut stream, _)) = listener.accept() {
                let _ = stream.set_nonblocking(false);
                let mut buf = [0; 1024];
                let _ = stream.read(&mut buf);
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    archive_c.len()
                );
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.write_all(&archive_c);
                let _ = stream.flush();
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    });

    let installer = ReleaseInstaller::new()
        .with_base_url(format!("http://127.0.0.1:{port}"))
        .with_assets(custom_asset);

    let res = installer.ensure_installed(&bin_dir, InstallPolicy::Download);
    stop.store(true, Ordering::Relaxed);

    assert!(res.is_ok(), "{res:?}");
    assert!(bin_dir.join("obscura").is_file());
    assert!(bin_dir.join("obscura-worker").is_file());
    let marker = fs::read_to_string(bin_dir.join(".obscura.sha256")).unwrap();
    assert!(marker.contains(&format!("archive={}", hash_leaked.to_ascii_lowercase())), "{marker}");
    assert!(marker.contains("obscura-worker="), "{marker}");

    // Verified fast path: untouched binaries are accepted without the archive.
    assert!(installer.ensure_installed(&bin_dir, InstallPolicy::PreinstalledOnly).is_ok());

    // A tampered binary next to a correct marker must not be trusted.
    fs::write(bin_dir.join("obscura"), b"tampered").unwrap();
    assert!(
        installer.ensure_installed(&bin_dir, InstallPolicy::PreinstalledOnly).is_err(),
        "tampered binary must fail re-verification"
    );
}

#[test]
fn hash_mismatch_rejects_and_leaves_no_files() {
    let tmp = tempdir().unwrap();
    let bin_dir = tmp.path().join("bin");

    let archive = make_tar_gz(&[("obscura", b"corrupted"), ("obscura-worker", b"worker")]);
    let custom_asset: &'static [(&'static str, &'static str, &'static str, &'static str, u64)] = Box::leak(Box::new([
        (std::env::consts::OS, std::env::consts::ARCH, "asset.tar.gz", "wrong_hash", archive.len() as u64),
    ]));

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_c = stop.clone();
    let archive_c = archive.clone();
    std::thread::spawn(move || {
        listener.set_nonblocking(true).unwrap();
        while !stop_c.load(Ordering::Relaxed) {
            if let Ok((mut stream, _)) = listener.accept() {
                let _ = stream.set_nonblocking(false);
                let mut buf = [0; 1024];
                let _ = stream.read(&mut buf);
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    archive_c.len()
                );
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.write_all(&archive_c);
                let _ = stream.flush();
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    });

    let installer = ReleaseInstaller::new()
        .with_base_url(format!("http://127.0.0.1:{port}"))
        .with_assets(custom_asset);

    let res = installer.ensure_installed(&bin_dir, InstallPolicy::Download);
    stop.store(true, Ordering::Relaxed);

    assert!(res.is_err());
    assert!(!bin_dir.join("obscura").exists());
    assert!(!bin_dir.join(".obscura.sha256").exists());
}

#[test]
fn archive_with_unexpected_or_symlink_entries_is_rejected() {
    let tmp = tempdir().unwrap();
    let bin_dir = tmp.path().join("bin");

    // Contains malicious/unexpected entry "hacked.sh"
    let archive = make_tar_gz(&[("obscura", b"bin"), ("obscura-worker", b"worker"), ("hacked.sh", b"rm -rf /")]);
    let hash = sha256_hex(&archive);
    let size = archive.len() as u64;

    let hash_leaked = Box::leak(hash.into_boxed_str()) as &'static str;
    let custom_asset: &'static [(&'static str, &'static str, &'static str, &'static str, u64)] = Box::leak(Box::new([
        (std::env::consts::OS, std::env::consts::ARCH, "asset.tar.gz", hash_leaked, size),
    ]));

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_c = stop.clone();
    let archive_c = archive.clone();
    std::thread::spawn(move || {
        listener.set_nonblocking(true).unwrap();
        while !stop_c.load(Ordering::Relaxed) {
            if let Ok((mut stream, _)) = listener.accept() {
                let _ = stream.set_nonblocking(false);
                let mut buf = [0; 1024];
                let _ = stream.read(&mut buf);
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    archive_c.len()
                );
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.write_all(&archive_c);
                let _ = stream.flush();
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    });

    let installer = ReleaseInstaller::new()
        .with_base_url(format!("http://127.0.0.1:{port}"))
        .with_assets(custom_asset);

    let res = installer.ensure_installed(&bin_dir, InstallPolicy::Download);
    stop.store(true, Ordering::Relaxed);

    assert!(res.is_err());
    assert!(!bin_dir.join("obscura").exists());
    assert!(!bin_dir.join("hacked.sh").exists());
}

#[test]
fn oversized_download_is_rejected_before_writing_everything() {
    let tmp = tempdir().unwrap();
    let bin_dir = tmp.path().join("bin");

    let archive = make_tar_gz(&[("obscura", b"bin"), ("obscura-worker", b"worker")]);
    let hash = sha256_hex(&archive);
    let size = archive.len() as u64;

    let hash_leaked = Box::leak(hash.into_boxed_str()) as &'static str;
    let custom_asset: &'static [(&'static str, &'static str, &'static str, &'static str, u64)] = Box::leak(Box::new([
        (std::env::consts::OS, std::env::consts::ARCH, "asset.tar.gz", hash_leaked, size),
    ]));

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_c = stop.clone();
    let archive_c = archive.clone();
    std::thread::spawn(move || {
        listener.set_nonblocking(true).unwrap();
        while !stop_c.load(Ordering::Relaxed) {
            if let Ok((mut stream, _)) = listener.accept() {
                let _ = stream.set_nonblocking(false);
                let mut buf = [0; 1024];
                let _ = stream.read(&mut buf);
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    archive_c.len()
                );
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.write_all(&archive_c);
                let _ = stream.flush();
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    });

    let installer = ReleaseInstaller::new()
        .with_base_url(format!("http://127.0.0.1:{port}"))
        .with_assets(custom_asset)
        .with_max_bytes(10); // artificially low max_bytes (10 bytes)

    let res = installer.ensure_installed(&bin_dir, InstallPolicy::Download);
    stop.store(true, Ordering::Relaxed);

    assert!(res.is_err());
    assert!(!bin_dir.join("obscura").exists());
}

#[test]
fn content_length_mismatch_is_rejected() {
    let tmp = tempdir().unwrap();
    let bin_dir = tmp.path().join("bin");

    let archive = make_tar_gz(&[("obscura", b"bin"), ("obscura-worker", b"worker")]);
    let hash = sha256_hex(&archive);

    let hash_leaked = Box::leak(hash.into_boxed_str()) as &'static str;
    let custom_asset: &'static [(&'static str, &'static str, &'static str, &'static str, u64)] = Box::leak(Box::new([
        (std::env::consts::OS, std::env::consts::ARCH, "asset.tar.gz", hash_leaked, 999999), // expected size 999999
    ]));

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_c = stop.clone();
    let archive_c = archive.clone();
    std::thread::spawn(move || {
        listener.set_nonblocking(true).unwrap();
        while !stop_c.load(Ordering::Relaxed) {
            if let Ok((mut stream, _)) = listener.accept() {
                let _ = stream.set_nonblocking(false);
                let mut buf = [0; 1024];
                let _ = stream.read(&mut buf);
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    archive_c.len()
                );
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.write_all(&archive_c);
                let _ = stream.flush();
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    });

    let installer = ReleaseInstaller::new()
        .with_base_url(format!("http://127.0.0.1:{port}"))
        .with_assets(custom_asset);

    let res = installer.ensure_installed(&bin_dir, InstallPolicy::Download);
    stop.store(true, Ordering::Relaxed);

    assert!(res.is_err());
    assert!(!bin_dir.join("obscura").exists());
}

#[test]
fn archive_entries_must_match_full_top_level_path() {
    let tmp = tempdir().unwrap();
    let bin_dir = tmp.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let archive = make_tar_gz(&[("nested/obscura", b"bin"), ("obscura-worker", b"worker")]);
    let hash = Box::leak(sha256_hex(&archive).into_boxed_str()) as &'static str;
    let size = archive.len() as u64;
    let custom_asset: &'static [(&'static str, &'static str, &'static str, &'static str, u64)] = Box::leak(Box::new([
        (std::env::consts::OS, std::env::consts::ARCH, "asset.tar.gz", hash, size),
    ]));
    fs::write(bin_dir.join("obscura.tar.gz"), &archive).unwrap();
    let installer = ReleaseInstaller::new().with_assets(custom_asset);
    let res = installer.ensure_installed(&bin_dir, InstallPolicy::PreinstalledOnly);
    assert!(matches!(res, Err(api::obscura_manager::InstallError::InvalidArchive(_))), "{res:?}");
    assert!(!bin_dir.join("obscura").exists());
}

#[test]
fn preinstalled_binaries_require_a_matching_manifest() {
    let tmp = tempdir().unwrap();
    let bin_dir = tmp.path().join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    fs::write(bin_dir.join("obscura"), b"bin").unwrap();
    fs::write(bin_dir.join("obscura-worker"), b"worker").unwrap();
    let installer = ReleaseInstaller::new();
    // No archive and no manifest: not installed.
    assert!(installer.ensure_installed(&bin_dir, InstallPolicy::PreinstalledOnly).is_err());
    // Wrong manifest: rejected.
    fs::write(
        bin_dir.join(api::obscura_manager::PREINSTALLED_MANIFEST),
        format!("{}  obscura\n{}  obscura-worker\n", sha256_hex(b"other"), sha256_hex(b"worker")),
    )
    .unwrap();
    assert!(installer.ensure_installed(&bin_dir, InstallPolicy::PreinstalledOnly).is_err());
    // Matching manifest: accepted, and the verified fast path keeps working.
    fs::write(
        bin_dir.join(api::obscura_manager::PREINSTALLED_MANIFEST),
        format!("{}  obscura\n{}  obscura-worker\n", sha256_hex(b"bin"), sha256_hex(b"worker")),
    )
    .unwrap();
    assert!(installer.ensure_installed(&bin_dir, InstallPolicy::PreinstalledOnly).is_ok());
    assert!(installer.ensure_installed(&bin_dir, InstallPolicy::PreinstalledOnly).is_ok());
    // Manifest-installed binaries are never accepted under the Download policy marker check
    // without re-verification; tampering is caught.
    fs::write(bin_dir.join("obscura"), b"tampered").unwrap();
    assert!(installer.ensure_installed(&bin_dir, InstallPolicy::PreinstalledOnly).is_err());
}

#[test]
fn reported_version_must_mention_the_pinned_release() {
    assert!(api::obscura_manager::version_matches("Obscura/0.2.3"));
    assert!(api::obscura_manager::version_matches("obscura v0.2.3"));
    assert!(!api::obscura_manager::version_matches("Chrome/120.0"));
}
