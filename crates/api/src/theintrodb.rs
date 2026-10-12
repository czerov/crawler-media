//! TheIntroDB v3 API client for querying intro, recap, and credits timestamps.

use serde::Deserialize;

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct IntroSegment {
    #[serde(rename = "type")]
    pub segment_type: String,
    pub start: f64,
    pub end: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct EpisodeSegments {
    #[serde(default)]
    pub segments: Vec<IntroSegment>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MarkerResult {
    pub intro_start_ms: Option<i64>,
    pub intro_end_ms: Option<i64>,
    pub outro_start_ms: Option<i64>,
    pub outro_end_ms: Option<i64>,
}

/// TheIntroDB API Client
#[derive(Clone)]
pub struct TheIntroDbClient {
    api_key: Option<String>,
    base_url: String,
}

/// 手写 `Debug`：`api_key` 是第三方 API 凭证，绝不能进日志。
///
/// 之前这里是 `#[derive(Debug)]`，任何一句 `debug!(?client)` 都会把 key
/// 打进日志与前端"系统日志"。`base_url` 保留原样，便于诊断。
impl std::fmt::Debug for TheIntroDbClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TheIntroDbClient")
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("base_url", &self.base_url)
            .finish()
    }
}

#[cfg(test)]
mod debug_tests {
    use super::TheIntroDbClient;

    #[test]
    fn debug_never_prints_the_api_key() {
        let client = TheIntroDbClient::new(Some("theintrodb-key-secret".into()));
        let rendered = format!("{client:?}");
        assert!(
            !rendered.contains("theintrodb-key-secret"),
            "Debug 不得输出 api_key: {rendered}"
        );
        assert!(rendered.contains("<redacted>"), "{rendered}");
        assert!(rendered.contains("api.theintrodb.org"), "{rendered}");

        let unconfigured = TheIntroDbClient::default();
        assert!(
            format!("{unconfigured:?}").contains("None"),
            "未配置 key 应可诊断"
        );
    }
}

impl Default for TheIntroDbClient {
    fn default() -> Self {
        Self::new(None)
    }
}

impl TheIntroDbClient {
    pub fn new(api_key: Option<String>) -> Self {
        Self {
            api_key,
            base_url: "https://api.theintrodb.org/v3".to_string(),
        }
    }

    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    /// Query markers for a TV episode by TMDB ID, season, and episode.
    pub fn get_episode_markers(
        &self,
        tmdb_id: &str,
        season: u32,
        episode: u32,
    ) -> Result<Option<MarkerResult>, String> {
        let Some(key) = &self.api_key else {
            tracing::warn!(
                tmdb_id,
                season,
                episode,
                "【片头片尾】TheIntroDB 未配置 API Key，跳过云端查询"
            );
            return Ok(None);
        };
        if key.trim().is_empty() {
            tracing::warn!(
                tmdb_id,
                season,
                episode,
                "【片头片尾】TheIntroDB API Key 为空，跳过云端查询"
            );
            return Ok(None);
        }
        let url = format!(
            "{}/shows/tmdb/{tmdb_id}/seasons/{season}/episodes/{episode}",
            self.base_url.trim_end_matches('/')
        );
        tracing::info!(
            tmdb_id,
            season,
            episode,
            "【片头片尾】正在查询 TheIntroDB 云端片头片尾时间戳"
        );
        let resp = crate::http_agent::call(|agent| {
            agent
                .get(&url)
                .header("X-API-Key", key)
                .header("Accept", "application/json")
                .call()
        })
        .map_err(|e| {
            tracing::warn!(tmdb_id, season, episode, error = %e, "【片头片尾】TheIntroDB 请求失败");
            e.to_string()
        })?;

        if resp.status() == 404 {
            tracing::info!(
                tmdb_id,
                season,
                episode,
                "【片头片尾】TheIntroDB 无该集数据（404）"
            );
            return Ok(None);
        }
        if resp.status() != 200 {
            return Err(format!("TheIntroDB returned status {}", resp.status()));
        }

        let body = resp.into_body().read_to_vec().map_err(|e| e.to_string())?;

        let res: EpisodeSegments = serde_json::from_slice(&body).map_err(|e| e.to_string())?;

        let mut out = MarkerResult::default();
        for seg in res.segments {
            match seg.segment_type.to_lowercase().as_str() {
                "intro" => {
                    out.intro_start_ms = Some((seg.start * 1000.0) as i64);
                    out.intro_end_ms = Some((seg.end * 1000.0) as i64);
                }
                "credits" | "outro" => {
                    out.outro_start_ms = Some((seg.start * 1000.0) as i64);
                    out.outro_end_ms = Some((seg.end * 1000.0) as i64);
                }
                _ => {}
            }
        }
        if out.intro_start_ms.is_some() || out.outro_start_ms.is_some() {
            tracing::info!(
                tmdb_id,
                season,
                episode,
                intro_start_ms = out.intro_start_ms,
                outro_start_ms = out.outro_start_ms,
                "【片头片尾】TheIntroDB 命中片头/片尾标记"
            );
            Ok(Some(out))
        } else {
            tracing::info!(
                tmdb_id,
                season,
                episode,
                "【片头片尾】TheIntroDB 返回数据但未含 intro/outro 段"
            );
            Ok(None)
        }
    }
}
