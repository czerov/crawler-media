use std::path::{Path, PathBuf};

use domain::Torrent;
use serde::Deserialize;

use crate::{Downloader, DownloaderError, PathMap, agent_for_url, apply_maps};

#[derive(Clone, PartialEq, Eq)]
pub struct QbitConfig {
    pub url: String,
    pub username: String,
    pub password: String,
    pub category: Option<String>,
    /// Save-path prefix mappings (container path -> host path).
    pub path_maps: Vec<PathMap>,
}

/// 手写 `Debug`：`password` 是下载器凭证，绝不能进日志。
///
/// 之前这里是 `#[derive(Debug)]`，任何一句 `debug!(?config)` 都会把 qBittorrent
/// 口令打进日志与前端"系统日志"。其余字段保留原样，便于诊断。
impl std::fmt::Debug for QbitConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QbitConfig")
            .field("url", &self.url)
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .field("category", &self.category)
            .field("path_maps", &self.path_maps)
            .finish()
    }
}

#[cfg(test)]
mod debug_tests {
    use super::QbitConfig;

    #[test]
    fn debug_never_prints_the_password() {
        let config = QbitConfig {
            url: "http://127.0.0.1:8080".into(),
            username: "admin".into(),
            password: "qbit-password-secret".into(),
            category: Some("movies".into()),
            path_maps: Vec::new(),
        };
        let rendered = format!("{config:?}");
        assert!(
            !rendered.contains("qbit-password-secret"),
            "Debug 不得输出 password: {rendered}"
        );
        assert!(rendered.contains("<redacted>"), "{rendered}");
        assert!(rendered.contains("127.0.0.1:8080"), "{rendered}");
        assert!(rendered.contains("admin"), "{rendered}");
    }
}

pub struct QbitDownloader {
    config: QbitConfig,
    cookie: String,
}

impl QbitDownloader {
    pub fn connect(config: QbitConfig) -> Result<Self, DownloaderError> {
        let body = format!(
            "username={}&password={}",
            form_enc(&config.username),
            form_enc(&config.password)
        );
        let login_url = join(&config.url, "/api/v2/auth/login");
        tracing::debug!(url = %login_url, user = %config.username, "正在连接 qBittorrent");
        let response = agent_for_url(&config.url, true)
            .post(&login_url)
            .header("Referer", &config.url)
            .header("Origin", config.url.trim_end_matches('/'))
            .header("Content-Type", "application/x-www-form-urlencoded")
            .send(body)
            .map_err(|e| {
                tracing::error!(url = %config.url, error = %e, "连接 qBittorrent 失败");
                http_err(e)
            })?;
        let cookie = response
            .headers()
            .get("set-cookie")
            .and_then(|v| v.to_str().ok())
            .and_then(|raw| raw.split(';').next())
            .unwrap_or("")
            .to_string();
        if cookie.is_empty() {
            tracing::error!(url = %config.url, "qBittorrent 登录未返回认证 Cookie");
            return Err(DownloaderError::Message(
                "qBittorrent login produced no cookie".into(),
            ));
        }
        tracing::info!(url = %config.url, user = %config.username, "qBittorrent 连接成功");
        Ok(Self { config, cookie })
    }
}

impl Downloader for QbitDownloader {
    fn add(&self, torrent: &Torrent) -> Result<(), DownloaderError> {
        self.add_resolved(torrent, &torrent.enclosure)
    }

    fn add_resolved(&self, torrent: &Torrent, download_url: &str) -> Result<(), DownloaderError> {
        self.add_with_options(torrent, download_url, None)
    }

    fn add_with_options(
        &self,
        torrent: &Torrent,
        download_url: &str,
        save_path: Option<&str>,
    ) -> Result<(), DownloaderError> {
        if self.exact_existing_hash(torrent)?.is_some() {
            return Ok(());
        }
        let tag = format!(
            "{},{}",
            crate::TASK_TAG,
            crate::ownership_tag(&torrent.enclosure)
        );
        const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";
        let mut form = ureq::unversioned::multipart::Form::new()
            .text("paused", "false")
            .text("stopped", "false")
            .text("tags", tag.as_str());
        if let Some(save_path) = save_path.filter(|path| !path.trim().is_empty()) {
            form = form.text("savepath", save_path);
        }
        let mut temp_path = None;
        let mut fallback_url = true;
        if download_url.starts_with("http://") || download_url.starts_with("https://") {
            if let Ok(bytes) = self.download_torrent_bytes(download_url, UA) {
                if bytes.len() >= 8 {
                    static NONCE: std::sync::atomic::AtomicU64 =
                        std::sync::atomic::AtomicU64::new(0);
                    let n = NONCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let temp = std::env::temp_dir().join(format!(
                        "cm-torrent-{}-{n}-{}.torrent",
                        std::process::id(),
                        tag
                    ));
                    if std::fs::write(&temp, &bytes).is_ok() {
                        temp_path = Some(temp);
                        fallback_url = false;
                    }
                }
            }
        }
        if let Some(ref path) = temp_path {
            if let Ok(part) = ureq::unversioned::multipart::Part::file(path) {
                form = form.part("torrents", part.file_name("download.torrent"));
            }
        } else if fallback_url {
            if download_url != torrent.enclosure {
                return Err(DownloaderError::Message(
                    "failed to fetch resolved Torrent before the signed URL expired".into(),
                ));
            }
            form = form.text("urls", download_url);
        }
        if let Some(category) = &self.config.category {
            form = form.text("category", category);
        }
        self.post_add_form(form, &torrent.title, &tag)
    }

    fn uploaded_by_category(&self, category: &str) -> Result<u64, DownloaderError> {
        let url = format!(
            "{}?category={}",
            join(&self.config.url, "/api/v2/torrents/info"),
            form_enc(category)
        );
        let body = self.get(&url)?;
        let infos: Vec<TorrentInfo> =
            serde_json::from_str(&body).map_err(|err| DownloaderError::Message(err.to_string()))?;
        Ok(infos.iter().map(|info| info.uploaded).sum())
    }

    fn remove(&self, torrent: &Torrent, delete_files: bool) -> Result<(), DownloaderError> {
        self.remove_owned(torrent, delete_files)
    }

    fn owned_identity(&self, torrent: &Torrent) -> Result<Option<String>, DownloaderError> {
        self.exact_existing_hash(torrent)
    }

    fn endpoint(&self) -> Option<String> {
        Some(format!(
            "qbittorrent|{}",
            self.config.url.trim_end_matches('/')
        ))
    }

    fn remove_owned(&self, torrent: &Torrent, delete_files: bool) -> Result<(), DownloaderError> {
        let Some(hash) = crate::owned::owned_removal_target(
            self.owned_identity(torrent),
            &torrent.enclosure,
            false,
        )?
        else {
            return Ok(());
        };
        tracing::info!(torrent = %torrent.title, hash = %hash, delete_files = %delete_files, "从 qBittorrent 移除已证实归属的种子");
        let url = join(&self.config.url, "/api/v2/torrents/delete");
        let body = format!("hashes={}&deleteFiles={}", form_enc(&hash), delete_files);
        let _ = self.post_form(&url, &body)?;
        Ok(())
    }

    fn completed_files(&self, torrent: &Torrent) -> Result<Vec<PathBuf>, DownloaderError> {
        let Some(info) = self.owned_info(torrent)? else {
            return Ok(Vec::new());
        };
        if info.progress < 1.0 {
            return Ok(Vec::new());
        }
        let files_url = format!(
            "{}?hash={}",
            join(&self.config.url, "/api/v2/torrents/files"),
            form_enc(&info.hash)
        );
        let file_body = self.get(&files_url)?;
        let listed: Vec<TorrentFile> = serde_json::from_str(&file_body)
            .map_err(|err| DownloaderError::Message(err.to_string()))?;
        Ok(listed
            .into_iter()
            .filter(|file| file.progress >= 1.0)
            .map(|file| Path::new(&info.save_path).join(file.name))
            .map(|path| apply_maps(&path, &self.config.path_maps))
            .collect())
    }

    fn delete_task(&self, info_hash: &str, delete_files: bool) -> Result<(), DownloaderError> {
        tracing::info!(hash = %info_hash, delete_files, "从 qBittorrent 删除任务");
        let url = join(&self.config.url, "/api/v2/torrents/delete");
        let body = format!(
            "hashes={}&deleteFiles={}",
            form_enc(info_hash),
            delete_files
        );
        let _ = self.post_form(&url, &body)?;
        Ok(())
    }

    fn pause_task(&self, info_hash: &str) -> Result<(), DownloaderError> {
        tracing::info!(hash = %info_hash, "暂停 qBittorrent 任务");
        let url = join(&self.config.url, "/api/v2/torrents/pause");
        let body = format!("hashes={}", form_enc(info_hash));
        let _ = self.post_form(&url, &body)?;
        Ok(())
    }

    fn resume_task(&self, info_hash: &str) -> Result<(), DownloaderError> {
        tracing::info!(hash = %info_hash, "恢复 qBittorrent 任务");
        let url = join(&self.config.url, "/api/v2/torrents/resume");
        let body = format!("hashes={}", form_enc(info_hash));
        let _ = self.post_form(&url, &body)?;
        Ok(())
    }

    fn get_speed_limits(&self) -> Result<(u64, u64), DownloaderError> {
        let prefs_url = join(&self.config.url, "/api/v2/app/preferences");
        let body = self.get(&prefs_url)?;
        let prefs: serde_json::Value =
            serde_json::from_str(&body).map_err(|err| DownloaderError::Message(err.to_string()))?;
        let dl = prefs["dl_limit"].as_u64().unwrap_or(0);
        let ul = prefs["up_limit"].as_u64().unwrap_or(0);
        Ok((dl, ul))
    }

    fn set_speed_limits(
        &self,
        download_limit: u64,
        upload_limit: u64,
    ) -> Result<(), DownloaderError> {
        tracing::info!(
            dl_limit = download_limit,
            up_limit = upload_limit,
            "设置 qBittorrent 速度限制"
        );
        let url = join(&self.config.url, "/api/v2/app/setPreferences");
        let payload = format!(
            "json={}",
            form_enc(&format!(
                "{{\"dl_limit\":{},\"up_limit\":{}}}",
                download_limit, upload_limit
            ))
        );
        let response = agent_for_url(&self.config.url, true)
            .post(&url)
            .header("Referer", &self.config.url)
            .header("Origin", self.config.url.trim_end_matches('/'))
            .header("Cookie", &self.cookie)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .send(&payload)
            .map_err(http_err)?;
        let _ = read_body(response)?;
        Ok(())
    }

    /// Enumerate every task. Our own deliveries carry the constant
    /// `crawler-media` tag (legacy per-torrent `crawler-media-*` tags from
    /// earlier builds are still recognized) and are matched by it; foreign
    /// torrents in the same client have no such tag and are reported too
    /// (the task center groups them as 刷流做种).
    fn task_snapshots(&self) -> Result<Vec<crate::TaskSnapshot>, DownloaderError> {
        let url = join(&self.config.url, "/api/v2/torrents/info");
        let body = self.get(&url)?;
        let infos: Vec<TorrentInfo> =
            serde_json::from_str(&body).map_err(|err| DownloaderError::Message(err.to_string()))?;
        Ok(infos
            .into_iter()
            .map(|info| {
                let tag = our_tag(&info.tags);
                let owned_tags = crate::owned_tags_from(info.tags.split(','));
                crate::TaskSnapshot {
                    owned_tags,
                    tag,
                    name: info.name,
                    progress: info.progress,
                    state: info.state,
                    size_bytes: info.size,
                    downloaded_bytes: info.downloaded,
                    uploaded_bytes: info.uploaded,
                    download_speed: info.dlspeed,
                    upload_speed: info.upspeed,
                    info_hash: info.hash,
                }
            })
            .collect())
    }
}

impl QbitDownloader {
    fn exact_existing_hash(&self, torrent: &Torrent) -> Result<Option<String>, DownloaderError> {
        let body = self.get(&join(&self.config.url, "/api/v2/torrents/info"))?;
        let infos: Vec<TorrentInfo> =
            serde_json::from_str(&body).map_err(|e| DownloaderError::Message(e.to_string()))?;
        let magnet = crate::magnet_info_hash(&torrent.enclosure);
        let tag = crate::ownership_tag(&torrent.enclosure);
        let hashes = infos
            .into_iter()
            .filter(|info| {
                if let Some(hash) = &magnet {
                    info.hash.eq_ignore_ascii_case(hash)
                } else {
                    info.tags.split(',').any(|item| item.trim() == tag)
                }
            })
            .map(|info| info.hash)
            .collect();
        crate::owned::proven_hashes(hashes)
    }

    fn owned_info(&self, torrent: &Torrent) -> Result<Option<TorrentInfo>, DownloaderError> {
        let Some(hash) = self.exact_existing_hash(torrent)? else {
            return Ok(None);
        };
        let url = join(&self.config.url, "/api/v2/torrents/info");
        let body = self.get(&url)?;
        let infos: Vec<TorrentInfo> =
            serde_json::from_str(&body).map_err(|err| DownloaderError::Message(err.to_string()))?;
        Ok(infos
            .into_iter()
            .find(|info| info.hash.eq_ignore_ascii_case(&hash)))
    }

    /// Download a tracker .torrent (or magnet) so the short-lived link does
    /// not expire before qBittorrent fetches it.
    fn download_torrent_bytes(
        &self,
        enclosure: &str,
        ua: &str,
    ) -> Result<Vec<u8>, DownloaderError> {
        let response = http_agent()
            .get(enclosure)
            .header("User-Agent", ua)
            .call()
            .map_err(http_err)?;
        response
            .into_body()
            .read_to_vec()
            .map_err(|err| DownloaderError::Message(err.to_string()))
    }

    fn get(&self, url: &str) -> Result<String, DownloaderError> {
        let response = agent_for_url(&self.config.url, true)
            .get(url)
            .header("Referer", &self.config.url)
            .header("Origin", self.config.url.trim_end_matches('/'))
            .header("Cookie", &self.cookie)
            .call()
            .map_err(http_err)?;
        read_body(response)
    }

    fn post_form(&self, url: &str, body: &str) -> Result<String, DownloaderError> {
        let response = agent_for_url(&self.config.url, true)
            .post(url)
            .header("Referer", &self.config.url)
            .header("Origin", self.config.url.trim_end_matches('/'))
            .header("Cookie", &self.cookie)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .send(body)
            .map_err(http_err)?;
        read_body(response)
    }

    fn post_add_form(
        &self,
        form: ureq::unversioned::multipart::Form<'_>,
        title: &str,
        tag: &str,
    ) -> Result<(), DownloaderError> {
        let url = join(&self.config.url, "/api/v2/torrents/add");
        let response = agent_for_url(&self.config.url, true)
            .post(&url)
            .header("Referer", &self.config.url)
            .header("Origin", self.config.url.trim_end_matches('/'))
            .header("Cookie", &self.cookie)
            .send(form)
            .map_err(|e| {
                tracing::error!(torrent = %title, error = %e, "提交种子到 qBittorrent 失败");
                http_err(e)
            })?;
        let body = read_body(response)?;
        if body.trim().eq_ignore_ascii_case("fails.") {
            tracing::error!(torrent = %title, "qBittorrent 拒绝了添加请求");
            return Err(DownloaderError::Message(
                "qBittorrent rejected the add".into(),
            ));
        }
        tracing::info!(torrent = %title, tag = %tag, "种子已成功添加到 qBittorrent");
        Ok(())
    }
}

fn http_agent() -> ureq::Agent {
    let config = ureq::config::Config::builder()
        .timeout_global(Some(std::time::Duration::from_secs(5)))
        .build();
    ureq::Agent::new_with_config(config)
}
fn read_body(response: ureq::http::Response<ureq::Body>) -> Result<String, DownloaderError> {
    response
        .into_body()
        .read_to_string()
        .map_err(|err| DownloaderError::Message(err.to_string()))
}

fn http_err(err: ureq::Error) -> DownloaderError {
    DownloaderError::Message(err.to_string())
}

fn join(base: &str, path: &str) -> String {
    format!("{}{path}", base.trim_end_matches('/'))
}

fn form_enc(value: &str) -> String {
    let mut out = String::new();
    for b in value.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[derive(Deserialize)]
struct TorrentInfo {
    hash: String,
    progress: f64,
    save_path: String,
    #[serde(default)]
    tags: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    uploaded: u64,
    #[serde(default)]
    category: String,
    /// Lifecycle string (`downloading`, `stalledDL`, `pausedDL`, `error`, …).
    #[serde(default)]
    state: String,
    #[serde(default)]
    downloaded: u64,
    #[serde(default)]
    dlspeed: u64,
    #[serde(default)]
    upspeed: u64,
}

#[derive(Deserialize)]
struct TorrentFile {
    name: String,
    progress: f64,
}

/// Pick our marker out of a client's comma-separated tag list.
///
/// Recognizes the constant [`crate::TASK_TAG`] plus any legacy per-torrent
/// `crawler-media-<hash>` tags still present in the client from earlier
/// builds; anything else (foreign torrents) yields "".
fn our_tag(tags: &str) -> String {
    let items: Vec<&str> = tags.split(',').map(str::trim).collect();
    if items.iter().any(|item| *item == crate::TASK_TAG) {
        return crate::TASK_TAG.to_string();
    }
    items
        .into_iter()
        .find(|item| {
            item.starts_with("crawler-media-") && !item.starts_with("crawler-media-owned-")
        })
        .unwrap_or("")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::our_tag;

    #[test]
    fn our_tag_recognizes_the_constant_tag() {
        assert_eq!(our_tag("crawler-media"), "crawler-media");
    }

    #[test]
    fn our_tag_recognizes_legacy_per_torrent_tags() {
        assert_eq!(
            our_tag("other,crawler-media-0000000000000001"),
            "crawler-media-0000000000000001"
        );
        assert_eq!(
            our_tag("crawler-media,crawler-media-owned-abcd"),
            "crawler-media"
        );
    }

    #[test]
    fn our_tag_ignores_foreign_and_empty_lists() {
        assert_eq!(our_tag(""), "");
        assert_eq!(our_tag("other,boost"), "");
        assert_eq!(our_tag("  other  ,  boost  "), "");
    }
}
