use std::fs;
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::Serialize;
use serde_json::Value;

#[path = "obscura_install.rs"]
mod install;
pub use install::{
    EXPECTED_ENTRIES, InstallError, MAX_ARCHIVE_BYTES, OBSCURA_ASSETS, OBSCURA_VERSION,
    ProcessLauncher, ReleaseInstaller,
};

pub const DEFAULT_OBSCURA_PORT: u16 = 9223;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallPolicy {
    Download,
    PreinstalledOnly,
}

impl InstallPolicy {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Download => "download",
            Self::PreinstalledOnly => "preinstalled",
        }
    }
}

impl std::str::FromStr for InstallPolicy {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "download" => Ok(Self::Download),
            "preinstalled" => Ok(Self::PreinstalledOnly),
            other => Err(format!("unknown install policy '{other}'")),
        }
    }
}

pub trait ObscuraInstaller: Send + Sync {
    fn ensure_installed(&self, bin_dir: &Path, policy: InstallPolicy) -> Result<PathBuf, InstallError>;
}

pub trait DaemonHandle: Send + Sync {
    fn pid(&self) -> u32;
    fn try_alive(&self) -> bool;
    fn kill(&self);
}

pub trait DaemonLauncher: Send + Sync {
    fn spawn(&self, binary: &Path, port: u16) -> Result<Box<dyn DaemonHandle>, String>;
}

#[derive(Clone, Debug, Serialize)]
pub struct ObscuraStatus {
    pub installed: bool,
    pub running: bool,
    pub downloading: bool,
    pub port: u16,
    pub cdp_url: Option<String>,
    pub expected_version: String,
    pub reported_version: Option<String>,
    pub binary_path: String,
    pub install_policy: String,
    pub last_error: Option<String>,
    pub not_started_reason: Option<String>,
}

type PortPicker = Arc<dyn Fn() -> u16 + Send + Sync>;

pub struct ObscuraManager {
    bin_dir: PathBuf,
    run_dir: PathBuf,
    port: Mutex<u16>,
    installer: Arc<dyn ObscuraInstaller>,
    launcher: Arc<dyn DaemonLauncher>,
    child: Arc<Mutex<Option<Box<dyn DaemonHandle>>>>,
    is_downloading: Arc<AtomicBool>,
    ready_timeout: Duration,
    port_picker: Option<PortPicker>,
    reported_version: Mutex<Option<String>>,
    last_error: Mutex<Option<String>>,
    not_started_reason: Mutex<Option<String>>,
    install_policy: Mutex<InstallPolicy>,
}

impl ObscuraManager {
    pub fn new(data_dir: &Path) -> Self {
        let bin_dir = data_dir.join("bin");
        let run_dir = data_dir.join("run");
        let _ = fs::create_dir_all(&run_dir);

        let manager = Self {
            bin_dir: bin_dir.clone(),
            run_dir: run_dir.clone(),
            port: Mutex::new(DEFAULT_OBSCURA_PORT),
            installer: Arc::new(ReleaseInstaller::new()),
            launcher: Arc::new(ProcessLauncher),
            child: Arc::new(Mutex::new(None)),
            is_downloading: Arc::new(AtomicBool::new(false)),
            ready_timeout: Duration::from_secs(10),
            port_picker: None,
            reported_version: Mutex::new(None),
            last_error: Mutex::new(None),
            not_started_reason: Mutex::new(None),
            install_policy: Mutex::new(InstallPolicy::PreinstalledOnly),
        };
        manager.reap_stale_pidfile();
        manager
    }

    pub fn with_installer(mut self, installer: Arc<dyn ObscuraInstaller>) -> Self {
        self.installer = installer;
        self
    }

    pub fn with_launcher(mut self, launcher: Arc<dyn DaemonLauncher>) -> Self {
        self.launcher = launcher;
        self
    }

    pub fn with_ready_timeout(mut self, timeout: Duration) -> Self {
        self.ready_timeout = timeout;
        self
    }

    pub fn with_port_picker<F>(mut self, picker: F) -> Self
    where
        F: Fn() -> u16 + Send + Sync + 'static,
    {
        self.port_picker = Some(Arc::new(picker));
        self
    }

    pub fn binary_path(&self) -> PathBuf {
        if cfg!(windows) {
            self.bin_dir.join("obscura.exe")
        } else {
            self.bin_dir.join("obscura")
        }
    }

    pub fn is_installed(&self) -> bool {
        let bin = self.binary_path();
        let hash = self.bin_dir.join(".obscura.sha256");
        bin.is_file() && hash.is_file()
    }

    pub fn is_downloading(&self) -> bool {
        self.is_downloading.load(Ordering::SeqCst)
    }

    pub fn owns_live_daemon(&self) -> bool {
        let mut guard = self.child.lock();
        if let Some(child) = guard.as_mut() {
            if child.try_alive() {
                true
            } else {
                *guard = None;
                false
            }
        } else {
            false
        }
    }

    pub fn is_running(&self) -> bool {
        let port = *self.port.lock();
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok()
    }

    pub fn port(&self) -> u16 {
        *self.port.lock()
    }

    /// Returns Some(cdp_url) ONLY when this manager owns a live, verified daemon.
    pub fn cdp_url(&self) -> Option<String> {
        let mut guard = self.child.lock();
        if let Some(child) = guard.as_mut() {
            if child.try_alive() {
                let port = *self.port.lock();
                return Some(format!("http://127.0.0.1:{port}"));
            } else {
                *guard = None;
            }
        }
        None
    }

    pub fn set_not_started_reason(&self, reason: Option<&'static str>) {
        *self.not_started_reason.lock() = reason.map(str::to_string);
    }

    pub fn set_last_error(&self, error: Option<String>) {
        *self.last_error.lock() = error;
    }

    pub fn status(&self) -> ObscuraStatus {
        let (running, port, cdp_url) = {
            let mut guard = self.child.lock();
            let running = if let Some(child) = guard.as_mut() {
                if child.try_alive() {
                    true
                } else {
                    *guard = None;
                    false
                }
            } else {
                false
            };
            let port = *self.port.lock();
            let cdp_url = if running {
                Some(format!("http://127.0.0.1:{port}"))
            } else {
                None
            };
            (running, port, cdp_url)
        };

        ObscuraStatus {
            installed: self.is_installed(),
            running,
            downloading: self.is_downloading(),
            port,
            cdp_url,
            expected_version: OBSCURA_VERSION.into(),
            reported_version: self.reported_version.lock().clone(),
            binary_path: self.binary_path().to_string_lossy().into(),
            install_policy: self.install_policy.lock().as_str().into(),
            last_error: self.last_error.lock().clone(),
            not_started_reason: self.not_started_reason.lock().clone(),
        }
    }

    pub fn start(&self, policy: InstallPolicy) -> Result<(), String> {
        *self.install_policy.lock() = policy;
        if self.owns_live_daemon() {
            return Ok(());
        }

        // Install
        if !self.is_installed() {
            if self
                .is_downloading
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
            {
                return Err("Obscura is currently downloading".into());
            }
            let res = self.installer.ensure_installed(&self.bin_dir, policy);
            self.is_downloading.store(false, Ordering::SeqCst);
            if let Err(e) = res {
                let err = e.to_string();
                *self.last_error.lock() = Some(err.clone());
                return Err(err);
            }
        }

        let bin = self.binary_path();
        let mut last_err = None;

        for attempt in 0..=2 {
            let port = if let Some(picker) = &self.port_picker {
                picker()
            } else {
                pick_free_port()
            };
            *self.port.lock() = port;

            tracing::info!(port, attempt, "Spawning Obscura daemon");
            let child = match self.launcher.spawn(&bin, port) {
                Ok(c) => c,
                Err(e) => {
                    last_err = Some(e);
                    continue;
                }
            };

            let start = Instant::now();
            let mut verified = false;
            let check_url = format!("http://127.0.0.1:{port}/json/version");
            let agent = ureq::Agent::config_builder()
                .timeout_connect(Some(Duration::from_millis(100)))
                .timeout_global(Some(Duration::from_millis(200)))
                .build()
                .new_agent();

            while start.elapsed() < self.ready_timeout {
                if !child.try_alive() {
                    break;
                }
                if let Ok(resp) = agent.get(&check_url).call() {
                    if let Ok(val) = resp.into_body().read_json::<Value>() {
                        if let Some(browser_str) = val.get("Browser").and_then(Value::as_str) {
                            *self.reported_version.lock() = Some(browser_str.to_string());
                            verified = true;
                            break;
                        }
                    }
                }
                std::thread::sleep(Duration::from_millis(50));
            }

            if verified && child.try_alive() {
                self.write_pidfile(child.pid(), port);
                *self.child.lock() = Some(child);
                *self.last_error.lock() = None;
                *self.not_started_reason.lock() = None;
                return Ok(());
            }

            child.kill();
            last_err = Some("Obscura process failed readiness probe or exited".into());
        }

        let err = last_err.unwrap_or_else(|| "Obscura failed to start".into());
        *self.last_error.lock() = Some(err.clone());
        Err(err)
    }

    pub fn stop(&self) {
        let mut guard = self.child.lock();
        if let Some(child) = guard.take() {
            child.kill();
        }
        self.remove_pidfile();
    }

    pub fn shutdown(&self) {
        self.stop();
    }

    fn pidfile_path(&self) -> PathBuf {
        self.run_dir.join("obscura.pid")
    }

    fn write_pidfile(&self, pid: u32, port: u16) {
        let _ = fs::create_dir_all(&self.run_dir);
        let nonce = uuid::Uuid::new_v4().simple();
        let content = format!("{pid} {port} {nonce}");
        let _ = fs::write(self.pidfile_path(), content);
    }

    fn remove_pidfile(&self) {
        let _ = fs::remove_file(self.pidfile_path());
    }

    fn reap_stale_pidfile(&self) {
        let path = self.pidfile_path();
        if let Ok(content) = fs::read_to_string(&path) {
            if let Some(pid_str) = content.split_whitespace().next() {
                if let Ok(pid) = pid_str.parse::<u32>() {
                    #[cfg(unix)]
                    {
                        // Check if pid is alive and belongs to obscura
                        let proc_exe = format!("/proc/{pid}/exe");
                        let is_obscura = if Path::new(&proc_exe).exists() {
                            fs::read_link(&proc_exe).map(|l| l == self.binary_path()).unwrap_or(false)
                        } else {
                            // On macOS or if /proc doesn't exist, check ps
                            let output = Command::new("ps")
                                .arg("-p")
                                .arg(pid.to_string())
                                .arg("-o")
                                .arg("comm=")
                                .output();
                            match output {
                                Ok(out) => {
                                    let comm = String::from_utf8_lossy(&out.stdout).trim().to_string();
                                    comm.contains("obscura")
                                }
                                Err(_) => false,
                            }
                        };
                        if is_obscura {
                            tracing::info!(pid, "Reaping orphaned managed Obscura daemon");
                            let _ = Command::new("kill").arg(pid.to_string()).output();
                        }
                    }
                }
            }
            let _ = fs::remove_file(path);
        }
    }
}

fn pick_free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .map(|addr| addr.port())
        .unwrap_or(DEFAULT_OBSCURA_PORT)
}
