//! Notify HTTP transport seam.
//!
//! 生产实现使用显式 resolve/connect/global 超时的 `ureq::Agent`；trait 本身是
//! 同步 blocking 的，调用方必须放进 blocking 预算（`ApiState::http_work`），
//! 因此 async worker 不会被网络等待占用。测试可注入阻塞/观测实现。

use std::time::Duration;

use serde_json::json;

/// 发送一条 notify 消息。实现必须能在 blocking 线程上安全执行。
pub trait NotifyTransport: Send + Sync {
    fn send(&self, base: &str, token: &str, title: &str, content: &str) -> Result<(), String>;
}

/// 生产实现：URL / headers / body 与既有 `notify::send_configured` 逐字一致。
pub struct HttpNotifyTransport;

impl HttpNotifyTransport {
    fn agent() -> ureq::Agent {
        ureq::Agent::config_builder()
            .timeout_resolve(Some(Duration::from_secs(3)))
            .timeout_connect(Some(Duration::from_secs(4)))
            .timeout_global(Some(Duration::from_secs(8)))
            .build()
            .new_agent()
    }
}

impl NotifyTransport for HttpNotifyTransport {
    fn send(&self, base: &str, token: &str, title: &str, content: &str) -> Result<(), String> {
        let app_id = base
            .rsplit('/')
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or("system_alerts")
            .to_string();
        let root = base.trim_end_matches(&format!("/{}", app_id));
        let url = format!("{root}/api/v1/notify/{app_id}");
        let payload = json!({
            "title": title,
            "content": content,
        });
        let _ = Self::agent()
            .post(&url)
            .header("Authorization", format!("Bearer {token}"))
            .header("Content-Type", "application/json")
            .send(payload.to_string())
            .map_err(|err| err.to_string())?;
        Ok(())
    }
}
