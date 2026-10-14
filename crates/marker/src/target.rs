use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::RwLock;

static CUSTOM_PROBE_UA: RwLock<Option<String>> = RwLock::new(None);

pub fn set_custom_probe_ua(ua: Option<String>) {
    *CUSTOM_PROBE_UA
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = ua;
}

pub fn active_probe_ua() -> String {
    CUSTOM_PROBE_UA
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
        .unwrap_or_else(|| DEFAULT_PROBE_UA.to_string())
}

pub const DEFAULT_PROBE_UA: &str = "crawler-media/0.1.0";
const PROXY_ENV_VARS: [&str; 6] = [
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProbeTarget {
    Local(PathBuf),
    Remote(String),
}

impl ProbeTarget {
    pub fn from_path(path: &Path) -> Self {
        if path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("strm"))
        {
            if let Some(url) = read_strm_url(path) {
                return Self::Remote(url);
            }
        }
        Self::Local(path.to_path_buf())
    }

    /// Apply this probe target as an input to ffprobe.
    pub fn apply_input(&self, cmd: &mut Command) {
        match self {
            Self::Local(path) => {
                cmd.arg(path);
            }
            Self::Remote(url) => {
                disable_proxy_environment(cmd);
                let ua = active_probe_ua();
                cmd.args([
                    "-user_agent",
                    &ua,
                    "-http_proxy",
                    "",
                    "-timeout",
                    "30000000",
                    "-analyzeduration",
                    "10000000",
                    "-probesize",
                    "10000000",
                ])
                .arg(url);
            }
        }
    }

    /// Apply this target as an ffmpeg input with fast input seeking.
    pub fn apply_ffmpeg_input_with_seek(&self, cmd: &mut Command, start_secs: u32) {
        self.apply_ffmpeg_input_with_seek_ms(cmd, (start_secs as i64) * 1000);
    }

    /// Apply this target as an ffmpeg input with decimal seek in milliseconds.
    pub fn apply_ffmpeg_input_with_seek_ms(&self, cmd: &mut Command, start_ms: i64) {
        let seek_str = if start_ms > 0 {
            if start_ms % 1000 == 0 {
                format!("{}", start_ms / 1000)
            } else {
                format!("{:.3}", start_ms as f64 / 1000.0)
            }
        } else {
            String::new()
        };

        match self {
            Self::Local(path) => {
                if start_ms > 0 {
                    cmd.args(["-ss", &seek_str]);
                }
                cmd.arg("-i").arg(path);
            }
            Self::Remote(url) => {
                disable_proxy_environment(cmd);
                let ua = active_probe_ua();
                cmd.args([
                    "-user_agent",
                    &ua,
                    "-http_proxy",
                    "",
                    "-timeout",
                    "30000000",
                    "-reconnect",
                    "1",
                    "-reconnect_streamed",
                    "1",
                    "-reconnect_on_network_error",
                    "1",
                    "-reconnect_at_eof",
                    "1",
                    "-reconnect_on_http_error",
                    "403,429,5xx",
                    "-reconnect_max_retries",
                    "3",
                    "-reconnect_delay_max",
                    "5",
                    "-reconnect_delay_total_max",
                    "15",
                    "-analyzeduration",
                    "10000000",
                    "-probesize",
                    "10000000",
                ]);
                if start_ms > 0 {
                    cmd.args(["-ss", &seek_str]);
                }
                cmd.args(["-i", url]);
            }
        }
    }

    /// Apply this target as an ffmpeg input. Input options must precede `-i`.
    pub fn apply_ffmpeg_input(&self, cmd: &mut Command) {
        self.apply_ffmpeg_input_with_seek(cmd, 0);
    }
}

fn disable_proxy_environment(command: &mut Command) {
    for variable in PROXY_ENV_VARS {
        command.env_remove(variable);
    }
}

pub fn read_strm_url(path: &Path) -> Option<String> {
    use std::io::Read;
    let file = std::fs::File::open(path).ok()?;
    let mut handle = file.take(65_537);
    let mut buf = Vec::new();
    handle.read_to_end(&mut buf).ok()?;
    if buf.len() > 65_536 {
        return None;
    }
    let content = String::from_utf8(buf).ok()?;
    content
        .lines()
        .map(|line| line.trim())
        .find(|line| {
            !line.is_empty() && (line.starts_with("http://") || line.starts_with("https://"))
        })
        .map(|s| s.to_string())
}
