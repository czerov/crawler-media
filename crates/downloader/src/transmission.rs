use std::path::{Path, PathBuf};

use domain::Torrent;
use serde::Deserialize;
use serde_json::{Value, json};
use ureq::typestate::WithBody;

use crate::{Downloader, DownloaderError, PathMap, agent_for_url, apply_maps};

#[derive(Clone, PartialEq, Eq)]
pub struct TransmissionConfig {
    pub url: String,
    pub username: Option<String>,
    pub password: Option<String>,
    /// Save-path prefix mappings (container path -> host path).
    pub path_maps: Vec<PathMap>,
}

/// 手写 `Debug`：`password` 是下载器凭证，绝不能进日志。
///
/// 之前这里是 `#[derive(Debug)]`，任何一句 `debug!(?config)` 都会把 Transmission
/// 口令打进日志与前端"系统日志"。`None`/`Some("<redacted>")` 仍能看出是否配置了
/// 口令，其余字段保留原样，便于诊断。
impl std::fmt::Debug for TransmissionConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TransmissionConfig")
            .field("url", &self.url)
            .field("username", &self.username)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .field("path_maps", &self.path_maps)
            .finish()
    }
}

#[cfg(test)]
mod debug_tests {
    use super::TransmissionConfig;

    #[test]
    fn debug_never_prints_the_password() {
        let config = TransmissionConfig {
            url: "http://127.0.0.1:9091".into(),
            username: Some("admin".into()),
            password: Some("transmission-password-secret".into()),
            path_maps: Vec::new(),
        };
        let rendered = format!("{config:?}");
        assert!(
            !rendered.contains("transmission-password-secret"),
            "Debug 不得输出 password: {rendered}"
        );
        assert!(rendered.contains("<redacted>"), "{rendered}");
        assert!(rendered.contains("127.0.0.1:9091"), "{rendered}");
        assert!(rendered.contains("admin"), "{rendered}");

        let anonymous = TransmissionConfig {
            url: "http://127.0.0.1:9091".into(),
            username: None,
            password: None,
            path_maps: Vec::new(),
        };
        assert!(
            format!("{anonymous:?}").contains("None"),
            "未配置口令应可诊断"
        );
    }
}

pub struct TransmissionDownloader {
    config: TransmissionConfig,
    session: parking_lot::Mutex<Option<String>>,
    identities: parking_lot::Mutex<std::collections::HashMap<String, String>>,
    labels_supported: bool,
}

impl TransmissionDownloader {
    fn claim_duplicate(&self, hash: &str, torrent: &Torrent) -> Result<(), DownloaderError> {
        let got = self.rpc(
            "torrent-get",
            json!({ "ids": [hash], "fields": ["labels"] }),
        )?;
        let mut labels: Vec<String> = got["torrents"][0]["labels"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();
        for wanted in [
            crate::TASK_TAG.to_string(),
            crate::ownership_tag(&torrent.enclosure),
            crate::CLAIMED_TAG.to_string(),
        ] {
            if !labels.contains(&wanted) {
                labels.push(wanted);
            }
        }
        self.rpc("torrent-set", json!({ "ids": [hash], "labels": labels }))?;
        tracing::info!(torrent = %torrent.title, %hash, "Transmission 已有同 hash 任务，补打标签认领");
        Ok(())
    }

    /// Review H-3: the task carries `CLAIMED_TAG` (unknown counts as claimed).
    fn hash_claimed(&self, hash: &str) -> bool {
        match self.rpc(
            "torrent-get",
            json!({ "ids": [hash], "fields": ["labels"] }),
        ) {
            Ok(got) => got["torrents"][0]["labels"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|v| v.as_str() == Some(crate::CLAIMED_TAG)),
            Err(error) => {
                tracing::warn!(%hash, %error, "无法确认任务是否为认领的用户任务，按认领处理（不删文件）");
                true
            }
        }
    }

    pub fn connect(config: TransmissionConfig) -> Result<Self, DownloaderError> {
        tracing::debug!(url = %config.url, "正在连接 Transmission RPC");
        let mut dl = Self {
            config,
            session: parking_lot::Mutex::new(None),
            identities: parking_lot::Mutex::new(std::collections::HashMap::new()),
            labels_supported: false,
        };
        let session = dl.rpc("session-get", json!({})).map_err(|e| {
            tracing::error!(url = %dl.config.url, error = %e, "连接 Transmission 失败");
            e
        })?;
        dl.labels_supported = session["rpc-version"].as_u64().is_some_and(|v| v >= 16);
        tracing::info!(url = %dl.config.url, "Transmission RPC 连接成功");
        Ok(dl)
    }

    fn live_hash(&self, hash: &str) -> Result<Option<String>, DownloaderError> {
        let rows = self.rpc("torrent-get", json!({"fields": ["hashString"]}))?;
        let torrents = rows["torrents"]
            .as_array()
            .ok_or_else(|| crate::owned::unproven("invalid Transmission task list"))?;
        let hashes = torrents
            .iter()
            .filter(|row| {
                row["hashString"]
                    .as_str()
                    .is_some_and(|live| live.eq_ignore_ascii_case(hash))
            })
            .filter_map(|row| row["hashString"].as_str().map(str::to_string))
            .collect();
        crate::owned::proven_hashes(hashes)
    }

    fn labeled_hash(&self, torrent: &Torrent) -> Result<Option<String>, DownloaderError> {
        if !self.labels_supported {
            return Ok(None);
        }
        let rows = self.rpc("torrent-get", json!({"fields": ["hashString", "labels"]}))?;
        let torrents = rows["torrents"]
            .as_array()
            .ok_or_else(|| crate::owned::unproven("invalid Transmission task list"))?;
        let tag = crate::ownership_tag(&torrent.enclosure);
        let hashes = torrents
            .iter()
            .filter(|row| {
                row["labels"]
                    .as_array()
                    .is_some_and(|labels| labels.iter().any(|label| label.as_str() == Some(&tag)))
            })
            .filter_map(|row| row["hashString"].as_str().map(str::to_string))
            .collect();
        crate::owned::proven_hashes(hashes)
    }

    fn rpc(&self, method: &str, arguments: Value) -> Result<Value, DownloaderError> {
        let payload = json!({ "method": method, "arguments": arguments }).to_string();
        let response = self.send(&payload)?;
        if let Some(session) = response
            .headers()
            .get("X-Transmission-Session-Id")
            .and_then(|value| value.to_str().ok())
        {
            *self.session.lock() = Some(session.to_string());
        }
        let body = response
            .into_body()
            .read_to_string()
            .map_err(|err| DownloaderError::Message(err.to_string()))?;
        let parsed: RpcResponse =
            serde_json::from_str(&body).map_err(|err| DownloaderError::Message(err.to_string()))?;
        if parsed.result != "success" {
            return Err(DownloaderError::Message(parsed.result));
        }
        Ok(parsed.arguments)
    }

    fn send(&self, payload: &str) -> Result<ureq::http::Response<ureq::Body>, DownloaderError> {
        let agent = agent_for_url(&self.config.url, false);
        let make_req = |session_id: Option<&str>| {
            let mut request = agent.post(&self.config.url);
            if let (Some(user), Some(pass)) = (&self.config.username, &self.config.password) {
                request = request.header(
                    "Authorization",
                    format!("Basic {}", base64_basic(user, pass)),
                );
            }
            if let Some(session) = session_id {
                request = request.header("X-Transmission-Session-Id", session);
            }
            request.header("Content-Type", "application/json")
        };

        let current_session = self.session.lock().clone();
        let resp = make_req(current_session.as_deref())
            .send(payload)
            .map_err(|err| DownloaderError::Message(err.to_string()))?;

        if resp.status().as_u16() == 409 {
            return self.retry_after_409(&resp, payload, &make_req);
        }

        if !resp.status().is_success() {
            let status = resp.status();
            tracing::error!(url = %self.config.url, %status, "Transmission 响应非成功状态");
            return Err(DownloaderError::Message(format!("HTTP error {status}")));
        }
        Ok(resp)
    }

    fn retry_after_409<F>(
        &self,
        resp: &ureq::http::Response<ureq::Body>,
        payload: &str,
        make_req: &F,
    ) -> Result<ureq::http::Response<ureq::Body>, DownloaderError>
    where
        F: Fn(Option<&str>) -> ureq::RequestBuilder<WithBody>,
    {
        let new_token = resp
            .headers()
            .get("X-Transmission-Session-Id")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
            .ok_or_else(|| {
                tracing::error!(url = %self.config.url, "Transmission 409 响应缺少 X-Transmission-Session-Id");
                DownloaderError::Message("missing X-Transmission-Session-Id on 409 response".into())
            })?;
        *self.session.lock() = Some(new_token.clone());
        let retry_resp = make_req(Some(&new_token))
            .send(payload)
            .map_err(|err| DownloaderError::Message(err.to_string()))?;
        if retry_resp.status().as_u16() == 409 {
            tracing::error!(url = %self.config.url, "带新 Session ID 重试后 Transmission 仍然返回 409");
            return Err(DownloaderError::Message(
                "Transmission rejected session challenge token".into(),
            ));
        }
        if !retry_resp.status().is_success() {
            let status = retry_resp.status();
            tracing::error!(url = %self.config.url, %status, "Transmission 重试响应非成功状态");
            return Err(DownloaderError::Message(format!("HTTP error {status}")));
        }
        Ok(retry_resp)
    }
}

impl Downloader for TransmissionDownloader {
    fn remember_identity(&self, torrent: &Torrent, hash: &str) -> Result<(), DownloaderError> {
        if let Some(hash) = crate::owned::normalize_hash(hash) {
            self.identities
                .lock()
                .entry(torrent.enclosure.clone())
                .or_insert(hash);
        }
        Ok(())
    }

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
        tracing::info!(torrent = %torrent.title, download_url, "添加种子到 Transmission");
        let mut arguments = json!({ "filename": download_url, "paused": false });
        if let Some(save_path) = save_path.filter(|path| !path.trim().is_empty()) {
            arguments["download-dir"] = json!(save_path);
        }
        if self.labels_supported {
            arguments["labels"] =
                json!([crate::TASK_TAG, crate::ownership_tag(&torrent.enclosure)]);
        }
        let added = self.rpc("torrent-add", arguments).map_err(|e| {
            tracing::error!(torrent = %torrent.title, error = %e, "添加种子到 Transmission 失败");
            e
        })?;
        // D-H2: a duplicate (e.g. added by hand) keeps its old labels; add ours
        // so ownership survives restarts. Existing labels are preserved.
        if self.labels_supported {
            if let Some(hash) = added["torrent-duplicate"]["hashString"].as_str() {
                self.claim_duplicate(hash, torrent)?;
            }
        }
        for field in ["torrent-added", "torrent-duplicate"] {
            if let Some(hash) = added[field]["hashString"]
                .as_str()
                .and_then(crate::owned::normalize_hash)
            {
                self.identities
                    .lock()
                    .insert(torrent.enclosure.clone(), hash);
            }
        }
        Ok(())
    }

    fn remove(&self, torrent: &Torrent, delete_files: bool) -> Result<(), DownloaderError> {
        self.remove_owned(torrent, delete_files)
    }

    fn owned_identity(&self, torrent: &Torrent) -> Result<Option<String>, DownloaderError> {
        if let Some(hash) = crate::magnet_info_hash(&torrent.enclosure) {
            return self.live_hash(&hash);
        }
        if let Some(hash) = self.identities.lock().get(&torrent.enclosure).cloned() {
            return self.live_hash(&hash);
        }
        self.labeled_hash(torrent)
    }

    fn select_files(
        &self,
        torrent: &Torrent,
        keep: &dyn Fn(&str) -> bool,
    ) -> Result<Option<usize>, DownloaderError> {
        let Some(hash) = self.owned_identity(torrent)? else {
            return Ok(None);
        };
        let arguments = self.rpc(
            "torrent-get",
            json!({ "ids": [hash.clone()], "fields": ["id", "hashString", "files"] }),
        )?;
        let Some(item) = arguments
            .get("torrents")
            .and_then(Value::as_array)
            .and_then(|items| {
                items.iter().find(|item| {
                    item.get("hashString")
                        .and_then(Value::as_str)
                        .is_some_and(|h| h.eq_ignore_ascii_case(&hash))
                })
            })
        else {
            return Ok(None);
        };
        let Some(listed) = item
            .get("files")
            .and_then(Value::as_array)
            .filter(|files| !files.is_empty())
        else {
            return Ok(None); // metadata not received yet
        };
        let names = listed
            .iter()
            .map(|file| file.get("name").and_then(Value::as_str).unwrap_or(""));
        if crate::selection_drops_all_videos(names, keep) {
            tracing::warn!(hash = %hash, "选择性下载会跳过全部视频文件，放弃筛选、整包下载");
            return Ok(Some(0));
        }
        let skipped: Vec<usize> = Some(listed)
            .map(|files| {
                files
                    .iter()
                    .enumerate()
                    .filter(|(_, file)| {
                        !keep(file.get("name").and_then(Value::as_str).unwrap_or(""))
                    })
                    .map(|(i, _)| i)
                    .collect()
            })
            .unwrap_or_default();
        if skipped.is_empty() {
            return Ok(Some(0));
        }
        let id = item.get("id").cloned().unwrap_or(Value::Null);
        tracing::info!(hash = %hash, count = skipped.len(), "Transmission 跳过不需要的文件");
        self.rpc(
            "torrent-set",
            json!({ "ids": [id], "files-unwanted": skipped }),
        )?;
        Ok(Some(skipped.len()))
    }

    fn endpoint(&self) -> Option<String> {
        Some(format!(
            "transmission|{}",
            self.config.url.trim_end_matches('/')
        ))
    }

    fn is_claimed(&self, torrent: &Torrent) -> Result<bool, DownloaderError> {
        Ok(match self.owned_identity(torrent)? {
            Some(hash) => self.hash_claimed(&hash),
            None => false,
        })
    }

    fn remove_owned(&self, torrent: &Torrent, delete_files: bool) -> Result<(), DownloaderError> {
        let known = self.identities.lock().contains_key(&torrent.enclosure);
        let Some(hash) = crate::owned::owned_removal_target(
            self.owned_identity(torrent),
            &torrent.enclosure,
            known,
        )?
        else {
            return Ok(());
        };
        let delete_files = delete_files && !self.hash_claimed(&hash);
        tracing::info!(torrent = %torrent.title, hash = %hash, delete_files, "从 Transmission 移除已证实归属的种子");
        self.rpc(
            "torrent-remove",
            json!({ "ids": [hash], "delete-local-data": delete_files }),
        )?;
        Ok(())
    }

    fn delete_task(&self, info_hash: &str, delete_files: bool) -> Result<(), DownloaderError> {
        let delete_files = delete_files && !self.hash_claimed(info_hash);
        tracing::info!(hash = %info_hash, delete_files, "从 Transmission 按已证实哈希删除任务");
        self.rpc(
            "torrent-remove",
            json!({ "ids": [info_hash], "delete-local-data": delete_files }),
        )?;
        Ok(())
    }

    fn pause_task(&self, info_hash: &str) -> Result<(), DownloaderError> {
        tracing::info!(hash = %info_hash, "从 Transmission 暂停任务");
        self.rpc("torrent-stop", json!({ "ids": [info_hash] }))?;
        Ok(())
    }

    fn resume_task(&self, info_hash: &str) -> Result<(), DownloaderError> {
        tracing::info!(hash = %info_hash, "从 Transmission 恢复任务");
        self.rpc("torrent-start", json!({ "ids": [info_hash] }))?;
        Ok(())
    }

    fn completed_files(&self, torrent: &Torrent) -> Result<Vec<PathBuf>, DownloaderError> {
        let (files, done) = self.collectable_files(torrent)?;
        Ok(if done { files } else { Vec::new() })
    }

    fn collectable_files(
        &self,
        torrent: &Torrent,
    ) -> Result<(Vec<PathBuf>, bool), DownloaderError> {
        let arguments = self.rpc(
            "torrent-get",
            json!({
                "fields": ["id", "name", "hashString", "sizeWhenDone", "percentDone", "downloadDir", "files", "fileStats"]
            }),
        )?;
        let empty = Vec::new();
        let torrents = arguments
            .get("torrents")
            .and_then(Value::as_array)
            .unwrap_or(&empty);
        let Some(owned_hash) = self.owned_identity(torrent)? else {
            return Ok((Vec::new(), false));
        };
        let matched: Vec<&Value> = torrents
            .iter()
            .filter(|item| {
                item.get("hashString")
                    .and_then(Value::as_str)
                    .is_some_and(|hash| hash.eq_ignore_ascii_case(&owned_hash))
            })
            .collect();
        if matched.is_empty() {
            return Ok((Vec::new(), false));
        }
        if matched.len() > 1 {
            tracing::error!(torrent = %torrent.title, matched_count = matched.len(), "匹配到多个任务存在歧义，拒绝收集文件");
            return Err(DownloaderError::Message(
                "ambiguous Transmission torrent identity".into(),
            ));
        }
        let target_item = matched[0];
        // percentDone covers the wanted files only.
        let done = target_item
            .get("percentDone")
            .and_then(Value::as_f64)
            .unwrap_or(0.0)
            >= 1.0;
        let mut files = Vec::new();
        extract_item_files(target_item, &self.config.path_maps, &mut files);
        Ok((files, done))
    }

    fn task_snapshots(&self) -> Result<Vec<crate::TaskSnapshot>, DownloaderError> {
        let mut fields = vec![
            "name",
            "hashString",
            "totalSize",
            "percentDone",
            "rateDownload",
            "rateUpload",
            "uploadedEver",
            "downloadedEver",
            "status",
        ];
        if self.labels_supported {
            fields.push("labels");
        }
        let response = self
            .rpc("torrent-get", json!({"fields": fields}))
            .map_err(|error| {
                tracing::error!(%error, "读取 Transmission 任务快照失败");
                error
            })?;
        let items = response["torrents"].as_array().ok_or_else(|| {
            tracing::error!("Transmission 任务快照缺少 torrents 数组");
            DownloaderError::Message("invalid Transmission task snapshot list".into())
        })?;
        let mut snapshots = Vec::new();
        for item in items {
            let name = item
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let info_hash = item
                .get("hashString")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let size_bytes = item.get("totalSize").and_then(Value::as_u64).unwrap_or(0);
            let tag = item["labels"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .find(|label| *label == crate::TASK_TAG || label.starts_with("crawler-media-"))
                .unwrap_or("")
                .to_string();
            let owned_tags = crate::owned_tags_from(
                item["labels"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str),
            );
            let progress = item
                .get("percentDone")
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
            let download_speed = item
                .get("rateDownload")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let upload_speed = item.get("rateUpload").and_then(Value::as_u64).unwrap_or(0);
            let downloaded_bytes = item
                .get("downloadedEver")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let uploaded_bytes = item
                .get("uploadedEver")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let tr_status = item.get("status").and_then(Value::as_i64).unwrap_or(0);
            // Transmission status: 0=stopped, 1=check_wait, 2=check, 3=download_wait, 4=download, 5=seed_wait, 6=seed
            let state = match tr_status {
                0 if progress >= 1.0 => "pausedUP".into(),
                0 => "pausedDL".into(),
                1 | 2 => "checkingDL".into(),
                3 | 4 => "downloading".into(),
                5 | 6 => "uploading".into(),
                _ => "unknown".into(),
            };
            snapshots.push(crate::TaskSnapshot {
                info_hash,
                name,
                progress,
                size_bytes,
                download_speed,
                upload_speed,
                downloaded_bytes,
                uploaded_bytes,
                state,
                tag,
                owned_tags,
            });
        }
        Ok(snapshots)
    }
}

fn extract_item_files(item: &Value, path_maps: &[PathMap], files: &mut Vec<PathBuf>) {
    let dir = item
        .get("downloadDir")
        .and_then(Value::as_str)
        .unwrap_or("");
    if let Some(listed) = item.get("files").and_then(Value::as_array) {
        let file_stats = item.get("fileStats").and_then(Value::as_array);
        for (i, file) in listed.iter().enumerate() {
            let bytes_completed = file
                .get("bytesCompleted")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let length = file.get("length").and_then(Value::as_u64).unwrap_or(0);
            // 验证是否被用户 deselect
            let wanted = file_stats
                .and_then(|stats| stats.get(i))
                .and_then(|stat| stat.get("wanted"))
                .and_then(Value::as_bool)
                .unwrap_or(true);
            if !wanted {
                continue;
            }
            // 未完成下载的文件不收集
            if length > 0 && bytes_completed < length {
                continue;
            }
            let name = file.get("name").and_then(Value::as_str).unwrap_or("");
            if !name.is_empty() {
                files.push(apply_maps(&Path::new(dir).join(name), path_maps));
            }
        }
    }
}

#[derive(Deserialize)]
struct RpcResponse {
    result: String,
    #[serde(default)]
    arguments: Value,
}

fn base64_basic(user: &str, pass: &str) -> String {
    encode(format!("{user}:{pass}").as_bytes())
}

fn encode(input: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in input.chunks(3) {
        let a = chunk[0];
        let b = chunk.get(1).copied().unwrap_or(0);
        let c = chunk.get(2).copied().unwrap_or(0);
        out.push(T[(a >> 2) as usize] as char);
        out.push(T[(((a & 3) << 4) | (b >> 4)) as usize] as char);
        if chunk.len() > 1 {
            out.push(T[(((b & 15) << 2) | (c >> 6)) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(T[(c & 63) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}
