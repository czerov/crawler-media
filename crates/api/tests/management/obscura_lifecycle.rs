use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use api::obscura_manager::{
    DaemonHandle, DaemonLauncher, InstallError, InstallPolicy, ObscuraInstaller, ObscuraManager,
};
use tempfile::tempdir;

static TEST_PORT_COUNTER: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(32100);
fn pick_test_port() -> u16 {
    TEST_PORT_COUNTER.fetch_add(1, Ordering::SeqCst)
}

#[test]
fn obscura_manager_paths_and_initial_status() {
    let tmp = tempdir().unwrap();
    let manager = ObscuraManager::new(tmp.path());

    assert_eq!(manager.cdp_url(), None);
    assert!(!manager.is_installed());
    assert!(!manager.is_downloading());
    assert_eq!(
        manager.binary_path(),
        tmp.path().join("bin").join(if cfg!(windows) {
            "obscura.exe"
        } else {
            "obscura"
        })
    );
}

struct FakeInstaller {
    succeed: bool,
    called: AtomicBool,
}
impl ObscuraInstaller for FakeInstaller {
    fn ensure_installed(&self, bin_dir: &Path, _policy: InstallPolicy) -> Result<PathBuf, InstallError> {
        self.called.store(true, Ordering::SeqCst);
        if !self.succeed {
            return Err(InstallError::Download("network error".into()));
        }
        let _ = fs::create_dir_all(bin_dir);
        let bin = bin_dir.join(if cfg!(windows) { "obscura.exe" } else { "obscura" });
        let _ = fs::write(&bin, b"#!/bin/sh\n");
        let _ = fs::write(bin_dir.join(".obscura.sha256"), "valid_hash");
        Ok(bin)
    }
}

struct FakeChild {
    alive: Arc<AtomicBool>,
    killed: Arc<AtomicBool>,
    pid: u32,
    listener_stop: Arc<AtomicBool>,
}
impl DaemonHandle for FakeChild {
    fn pid(&self) -> u32 { self.pid }
    fn try_alive(&self) -> bool { self.alive.load(Ordering::SeqCst) }
    fn kill(&self) {
        self.alive.store(false, Ordering::SeqCst);
        self.killed.store(true, Ordering::SeqCst);
        self.listener_stop.store(true, Ordering::SeqCst);
    }
}
impl Drop for FakeChild {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::SeqCst);
        self.listener_stop.store(true, Ordering::SeqCst);
    }
}

struct FakeLauncher {
    respond_version: bool,
    spawn_count: Arc<AtomicUsize>,
    killed: Arc<AtomicBool>,
    child_alive: Arc<AtomicBool>,
}
impl DaemonLauncher for FakeLauncher {
    fn spawn(&self, _binary: &Path, port: u16) -> Result<Box<dyn DaemonHandle>, String> {
        self.spawn_count.fetch_add(1, Ordering::SeqCst);
        let alive = self.child_alive.clone();
        alive.store(true, Ordering::SeqCst);
        let killed = self.killed.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = stop.clone();

        if self.respond_version {
            let listener = match TcpListener::bind(format!("127.0.0.1:{port}")) {
                Ok(l) => l,
                Err(e) => return Err(format!("port {port} in use: {e}")),
            };
            let _ = listener.set_nonblocking(true);
            std::thread::spawn(move || {
                while !stop_thread.load(Ordering::Relaxed) {
                    if let Ok((mut stream, _)) = listener.accept() {
                        let mut buf = [0; 1024];
                        let _ = stream.read(&mut buf);
                        let body = r#"{"Browser":"Obscura/0.2.3"}"#;
                        let resp = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        let _ = stream.write_all(resp.as_bytes());
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            });
        }

        Ok(Box::new(FakeChild {
            alive,
            killed,
            pid: 99999,
            listener_stop: stop,
        }))
    }
}

#[test]
fn start_installs_then_spawns_and_reports_running() {
    let tmp = tempdir().unwrap();
    let installer = Arc::new(FakeInstaller {
        succeed: true,
        called: AtomicBool::new(false),
    });
    let killed = Arc::new(AtomicBool::new(false));
    let launcher = Arc::new(FakeLauncher {
        respond_version: true,
        spawn_count: Arc::new(AtomicUsize::new(0)),
        killed,
        child_alive: Arc::new(AtomicBool::new(false)),
    });

    let manager = ObscuraManager::new(tmp.path())
        .with_installer(installer.clone())
        .with_launcher(launcher.clone())
        .with_port_picker(pick_test_port)
        .with_ready_timeout(Duration::from_millis(500));

    let res = manager.start(InstallPolicy::Download);
    assert!(res.is_ok());
    assert!(installer.called.load(Ordering::SeqCst));
    assert!(manager.owns_live_daemon());
    assert!(manager.cdp_url().is_some());
    // Version now comes from `obscura --version` (DaemonLauncher::version); the fake has none.
    assert_eq!(manager.status().reported_version.as_deref(), None);

    manager.stop();
    assert!(!manager.owns_live_daemon());
}

#[test]
fn start_kills_the_child_and_errors_when_json_version_never_answers() {
    let tmp = tempdir().unwrap();
    let installer = Arc::new(FakeInstaller {
        succeed: true,
        called: AtomicBool::new(false),
    });
    let killed = Arc::new(AtomicBool::new(false));
    let launcher = Arc::new(FakeLauncher {
        respond_version: false,
        spawn_count: Arc::new(AtomicUsize::new(0)),
        killed: killed.clone(),
        child_alive: Arc::new(AtomicBool::new(false)),
    });

    let manager = ObscuraManager::new(tmp.path())
        .with_installer(installer)
        .with_launcher(launcher)
        .with_port_picker(pick_test_port)
        .with_ready_timeout(Duration::from_millis(100));

    let res = manager.start(InstallPolicy::Download);
    assert!(res.is_err());
    assert!(killed.load(Ordering::SeqCst));
    assert!(!manager.owns_live_daemon());
    assert!(manager.cdp_url().is_none());
}

#[test]
fn stop_only_kills_the_process_this_manager_spawned() {
    let tmp = tempdir().unwrap();
    let manager = ObscuraManager::new(tmp.path());
    manager.stop();
    assert!(!manager.owns_live_daemon());
}

#[test]
fn exited_child_reports_not_running_and_no_cdp_url() {
    let tmp = tempdir().unwrap();
    let installer = Arc::new(FakeInstaller {
        succeed: true,
        called: AtomicBool::new(false),
    });
    let child_alive = Arc::new(AtomicBool::new(true));
    let launcher = Arc::new(FakeLauncher {
        respond_version: true,
        spawn_count: Arc::new(AtomicUsize::new(0)),
        killed: Arc::new(AtomicBool::new(false)),
        child_alive: child_alive.clone(),
    });

    let manager = ObscuraManager::new(tmp.path())
        .with_installer(installer)
        .with_launcher(launcher)
        .with_port_picker(pick_test_port)
        .with_ready_timeout(Duration::from_millis(500));

    manager.start(InstallPolicy::Download).unwrap();
    assert!(manager.owns_live_daemon());

    // Process exits unexpectedly
    child_alive.store(false, Ordering::SeqCst);
    assert!(!manager.owns_live_daemon());
    assert_eq!(manager.cdp_url(), None);
}

#[test]
fn stale_pidfile_for_a_foreign_process_is_removed_without_killing() {
    let tmp = tempdir().unwrap();
    let run_dir = tmp.path().join("run");
    fs::create_dir_all(&run_dir).unwrap();
    fs::write(run_dir.join("obscura.pid"), "1 9223 nonce").unwrap();

    let _manager = ObscuraManager::new(tmp.path());
    assert!(!run_dir.join("obscura.pid").exists());
}

#[test]
fn install_failure_is_reported_without_recording_a_child() {
    let tmp = tempdir().unwrap();
    let installer = Arc::new(FakeInstaller {
        succeed: false,
        called: AtomicBool::new(false),
    });
    let launcher = Arc::new(FakeLauncher {
        respond_version: true,
        spawn_count: Arc::new(AtomicUsize::new(0)),
        killed: Arc::new(AtomicBool::new(false)),
        child_alive: Arc::new(AtomicBool::new(false)),
    });

    let manager = ObscuraManager::new(tmp.path())
        .with_installer(installer)
        .with_launcher(launcher);

    assert!(manager.start(InstallPolicy::Download).is_err());
    assert!(!manager.owns_live_daemon());
    assert!(manager.status().last_error.is_some());
}

struct RaceLauncher {
    spawn_count: Arc<AtomicUsize>,
    killed: Arc<AtomicBool>,
}
impl DaemonLauncher for RaceLauncher {
    fn spawn(&self, _binary: &Path, port: u16) -> Result<Box<dyn DaemonHandle>, String> {
        let count = self.spawn_count.fetch_add(1, Ordering::SeqCst);
        if count == 0 {
            return Err("simulated port collision".into());
        }
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = stop.clone();
        let listener = match TcpListener::bind(format!("127.0.0.1:{port}")) {
            Ok(l) => l,
            Err(e) => return Err(format!("port {port} in use: {e}")),
        };
        listener.set_nonblocking(true).unwrap();
        std::thread::spawn(move || {
            while !stop_thread.load(Ordering::Relaxed) {
                if let Ok((mut stream, _)) = listener.accept() {
                    let _ = stream.set_nonblocking(false);
                    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
                    let mut buf = [0; 1024];
                    let _ = stream.read(&mut buf);
                    let body = r#"{"Browser":"Obscura/0.2.3"}"#;
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(resp.as_bytes());
                    let _ = stream.flush();
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        });

        Ok(Box::new(FakeChild {
            alive: Arc::new(AtomicBool::new(true)),
            killed: self.killed.clone(),
            pid: 99999,
            listener_stop: stop,
        }))
    }
}

#[test]
fn port_race_retries_with_a_new_port() {
    let tmp = tempdir().unwrap();
    let installer = Arc::new(FakeInstaller {
        succeed: true,
        called: AtomicBool::new(false),
    });
    let spawn_count = Arc::new(AtomicUsize::new(0));
    let launcher = Arc::new(RaceLauncher {
        spawn_count: spawn_count.clone(),
        killed: Arc::new(AtomicBool::new(false)),
    });

    let manager = ObscuraManager::new(tmp.path())
        .with_installer(installer)
        .with_launcher(launcher)
        .with_port_picker(pick_test_port)
        .with_ready_timeout(Duration::from_millis(500));

    assert!(manager.start(InstallPolicy::Download).is_ok());
    assert_eq!(spawn_count.load(Ordering::SeqCst), 2);
}

/// Real managed lifecycle against the pinned release archive (T0).
/// `OBSCURA_ARCHIVE=/path/obscura-x86_64-linux.tar.gz cargo test -p api --test management real_obscura -- --ignored`
#[test]
#[ignore = "needs the pinned Obscura release archive (T0)"]
fn real_obscura_managed_start_survives_spawn_thread_and_stops() {
    let archive = std::env::var("OBSCURA_ARCHIVE").expect("OBSCURA_ARCHIVE");
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("bin")).unwrap();
    std::fs::copy(archive, tmp.path().join("bin/obscura.tar.gz")).unwrap();
    let manager = std::sync::Arc::new(api::obscura_manager::ObscuraManager::new(tmp.path()));
    let m = manager.clone();
    std::thread::spawn(move || m.start(api::obscura_manager::InstallPolicy::PreinstalledOnly))
        .join()
        .unwrap()
        .expect("start");
    let status = manager.status();
    assert!(status.running, "{status:?}");
    assert_eq!(status.reported_version.as_deref(), Some("obscura 0.2.3"));
    assert!(status.last_error.is_none(), "{status:?}");
    std::thread::sleep(std::time::Duration::from_secs(12));
    assert!(manager.owns_live_daemon(), "daemon must outlive the spawning thread");
    let url = manager.cdp_url().expect("cdp url");
    assert!(url.starts_with("http://127.0.0.1:"));
    manager.stop();
    assert!(!manager.owns_live_daemon());
}
