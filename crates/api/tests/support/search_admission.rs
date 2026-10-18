//! 搜索预算准入的 public-seam fixture：可阻塞的搜索后端 + 多个启用站点。
//!
//! 站点搜索是 blocking HTTP，测试用一个受闸门的 `Fetcher` 占住搜索 permit，
//! 从 HTTP 响应观察“超预算站点降级为单站失败”和“客户端断开后不再排新站点”。

use std::sync::{Arc, Condvar, Mutex as StdMutex};
use std::time::{Duration, Instant};

use api::management::{ApiState, router};
use axum::body::Body;
use axum::http::Request;
use axum::response::Response;
use domain::{Site, SiteId};
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use tower::ServiceExt;

/// 阻塞式搜索后端：记录已开始/已完成的站点抓取，直到 `release` 才返回。
#[derive(Default)]
pub struct GatedSearchFetcher {
    started: StdMutex<Vec<String>>,
    completed: StdMutex<usize>,
    open: StdMutex<bool>,
    open_cv: Condvar,
}

impl GatedSearchFetcher {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn started(&self) -> Vec<String> {
        self.started.lock().unwrap().clone()
    }

    pub fn completed(&self) -> usize {
        *self.completed.lock().unwrap()
    }

    pub fn release(&self) {
        *self.open.lock().unwrap() = true;
        self.open_cv.notify_all();
    }

    /// 轮询等待至少 `count` 个站点抓取开始；不阻塞 async worker。
    pub async fn wait_started(&self, count: usize, deadline: Duration) -> bool {
        let until = Instant::now() + deadline;
        while Instant::now() < until {
            if self.started.lock().unwrap().len() >= count {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        false
    }

    /// 轮询等待至少 `count` 个站点抓取结束。
    pub async fn wait_completed(&self, count: usize, deadline: Duration) -> bool {
        let until = Instant::now() + deadline;
        while Instant::now() < until {
            if self.completed() >= count {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        false
    }
}

impl Fetcher for GatedSearchFetcher {
    fn fetch(&self, request: &FetchRequest) -> Result<String, IndexerError> {
        self.started.lock().unwrap().push(request.url.clone());
        let mut open = self.open.lock().unwrap();
        while !*open {
            open = self.open_cv.wait(open).unwrap();
        }
        *self.completed.lock().unwrap() += 1;
        // 合法的空列表页：站点成功返回 0 条，不制造额外失败。
        Ok("<html><body></body></html>".to_string())
    }
}

/// 断言失败/提前返回时也放行阻塞后端，避免 runtime 关闭时卡在 blocking 线程上。
pub struct ReleaseOnDrop(pub Arc<GatedSearchFetcher>);

impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        self.0.release();
    }
}

pub struct SearchAdmissionFixture {
    pub dir: tempfile::TempDir,
    pub state: ApiState,
    pub fetcher: Arc<GatedSearchFetcher>,
    pub admin_token: String,
    pub site_ids: Vec<String>,
}

impl SearchAdmissionFixture {
    /// 建一个启用 `site_count` 个 demo profile 站点、搜索后端阻塞的状态。
    pub fn new(site_count: usize) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = store::Store::open(dir.path().join("data")).unwrap();
        let admin_token = "management-secret".to_string();
        let fetcher = Arc::new(GatedSearchFetcher::new());
        let state = ApiState::new(
            store,
            admin_token.clone(),
            ProfileSet::load(None).unwrap(),
            fetcher.clone(),
            Arc::new(downloader::MemoryDownloader::new(dir.path().join("stage"))),
            dir.path().join("library"),
        )
        .unwrap();
        let mut site_ids = Vec::new();
        {
            let store = state.store();
            let guard = store.lock();
            for index in 0..site_count {
                let site = Site {
                    id: SiteId::new(),
                    name: format!("site-{index}"),
                    url: format!("https://site-{index}.invalid/"),
                    profile_id: "demo".into(),
                    cookie: None,
                    api_key: None,
                    rss_url: None,
                    proxy: None,
                    rate_limit_per_minute: None,
                    cdp_url: None,
                    downloader_id: None,
                    enabled: true,
                    maintenance_via: domain::MaintenanceVia::Http,
                };
                site_ids.push(site.id.to_string());
                guard.insert_site(&site).unwrap();
            }
        }
        Self {
            dir,
            state,
            fetcher,
            admin_token,
            site_ids,
        }
    }

    pub fn app(&self) -> axum::Router {
        router(self.state.clone())
    }

    /// 带管理员 token 的 GET；返回原始响应以便观察 SSE body 生命周期。
    pub async fn send(&self, uri: &str) -> Response {
        let request = Request::builder()
            .method("GET")
            .uri(uri)
            .header("Authorization", format!("Bearer {}", self.admin_token))
            .body(Body::empty())
            .unwrap();
        self.app().oneshot(request).await.unwrap()
    }

    /// 只搜索指定下标的站点，用于精确控制请求占用的 permit 数。
    pub fn sites_query(&self, indexes: &[usize]) -> String {
        indexes
            .iter()
            .map(|index| self.site_ids[*index].clone())
            .collect::<Vec<_>>()
            .join(",")
    }
}
