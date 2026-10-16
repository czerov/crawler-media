# CDP/Obscura 受管浏览器链路 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让实例级单一 CDP 开关成立：开启时按 Site `cdp_url` → 实例 `cdp.url` → 受管 Obscura 解析端点，任一 CDP 环节失败回退 HTTP；关闭时完全不使用 CDP；登录与签到可走浏览器并持久化真实回读的 cookie。

**Architecture:** 路由决策全部下沉到 `crates/indexer` 的 `BrowserConfig::resolve` 与 `RoutedFetcher`，Indexer 仍不依赖 API/Store；`crates/api` 通过 provider closure 在每次请求读取实时设置，并把受管 Obscura 的本地端点作为第三级兜底注入。登录/签到的 transport 选择与失败回退由 `crates/api` 的新模块统一编排，业务判定复用 `crates/hooks` 既有的 `SiteResponsePolicy`，因此 transport 不影响「签到成功」的结论。

**Tech Stack:** Rust 2024 workspace（`indexer` / `hooks` / `api`）、axum、rusqlite、tungstenite、thiserror、tracing；前端 Vite + React + `node --test`。

**Spec:** `docs/adr/0007-cdp-browser-opt-in.md`（决策）、`CONTEXT.md` 的 **Browser** 词条、`docs/deploy.md` 的部署边界。执行前先读这三处。

## 修订记录

- 2026-10-16 r2：按审核报告（B1–B2、H1–H5、M1–M7、L1–L8）修订。主要变化：新增 Task 0 兼容性 spike；Task 1 改为自足的完整实现步骤（不再依赖本机补丁）；cookie 注入改为 `Network.setCookies` 并按 host 绑定；受管 Obscura 改为哈希校验安装、随机端口 + 身份确认、按需启动、Job 化 reconcile；回退语义在数据层可见。

## 起点

分支 `feat/cdp-obscura` 自 `d201fcf` 起，代码与 `main` 相同。**不存在、也不依赖任何本机补丁**；Task 1 从现有代码（`crates/indexer/src/browser.rs` 中 `PageSession::cookie_header` 默认返回 `Ok("session=browser")`、`cdp_page.rs` 未实现 `cookie_header`、`set_cookie_header` 走 `Network.setExtraHTTPHeaders`）直接改起。

依赖链：T0 → T1 → T2 → T3；T4 → T5 → T6；T1 → T7 → T8（依赖 T6 的 `ApiState.browser`）；T9 依赖 T2、T4；T10 依赖 T2、T5。T0 的结论若否定某个 CDP 方法，先改 T1 的降级方式再开工。

### Task 0: Obscura CDP 兼容性 spike（手动 / `#[ignore]`，不进 CI）

现有客户端依赖的 CDP 能力在 Obscura（非 Chromium）上未经验证。本 task 产出事实，不产出生产代码。

**Files:**
- Create: `crates/indexer/tests/obscura_compat.rs`（全部 `#[ignore]`，读 `OBSCURA_COMPAT_URL` 环境变量；未设置即 skip）
- Create: `crates/indexer/tests/fixtures/render/js_list.html`（`DOMContentLoaded` 后由 JS 插入 `<table class="torrents">` 的本地页面）
- Modify: `docs/adr/0007-cdp-browser-opt-in.md`「验证边界」

**静态事实（2026-10-16 对 v0.2.3 x86_64-linux 资产的检查，未执行）：** 归档含 `obscura` 与 `obscura-worker` 两个文件；二进制字符串含 `invalid --host '`、`CDP server: ws://127.0.0.1:`，推测支持 `--host` 且默认绑定回环——以 spike 实测为准。

- [ ] **Step 1: 按 Task 4 的校验方式取得二进制**

在一台有网络的机器上下载 `obscura-<arch>-<os>.tar.gz`，`sha256sum` 必须与 Task 4 的 `OBSCURA_ASSETS` 表一致，否则停止。

- [ ] **Step 2: 启动并确认监听地址**

```bash
./obscura serve --host 127.0.0.1 --port 0   # 若不支持 0，用随机空闲端口
ss -ltnp | grep obscura                      # 必须只有 127.0.0.1
curl -s http://127.0.0.1:$PORT/json/version
```
记录：`--host` 是否生效、不传时默认地址、`--port 0` 是否支持及如何得知实际端口、`/json/version` 字段（`Browser` 版本串用于 L2）。

- [ ] **Step 3: 逐项验证方法**

`obscura_compat.rs` 中每个方法一个 `#[ignore]` 测试，用本地 `python3 -m http.server` 服务 fixture：
`PUT /json/new`（及 GET 回退）、`PUT /json/close/{id}`、`webSocketDebuggerUrl` 的 scheme/host/port、`Network.enable`、`Page.enable`、`Page.setLifecycleEventsEnabled` + `Page.lifecycleEvent(load)`、`Page.navigate`（`frameId`/`loaderId`）、`Runtime.evaluate`（`returnByValue`）、`Network.setCookies`、`Network.getCookies{urls}`、`Network.clearBrowserCookies`、`Target.createBrowserContext` / `Target.disposeBrowserContext`、JS 插入的表格能否在 `outerHTML` 中读到。另测一次 NexusPHP 签到页：浏览器 GET `attendance.php` 是否真实签到（L7）。

- [ ] **Step 4: 写入 ADR「验证边界」**

表格：方法 → 支持/不支持 → 降级方式。约定的降级：
- 无 `Target.createBrowserContext` → 每会话结束 `Network.clearBrowserCookies`，并由 Task 1 的信号量保证同一时刻同一 daemon 只有一个 Site 的 cookie（`max_sessions=1`）。
- 无 `Network.getCookies` → `cookie_header` 返回 `Err`，`RenderedPage.cookie_header = None`（保留原 cookie）。
- 无 `Network.setCookies` → **不渲染带 cookie 的请求**（报 `CdpUnsupported`，回退 HTTP），绝不退回 `setExtraHTTPHeaders`。
- 无 lifecycle 事件 → 仅以 `document.readyState == complete` 判定。
- 浏览器 GET 不真实签到 → Task 7 浏览器签到改为在页面内提交签到表单/按钮（`Runtime.evaluate` 触发 `form.submit()`），或该站点禁用浏览器签到。

若关键方法（`/json/new`、`Page.navigate`、`Runtime.evaluate`）任一不支持，**停止本计划**，回到 ADR 重新决策。

- [ ] **Step 5: 提交**

```bash
git add crates/indexer/tests/obscura_compat.rs crates/indexer/tests/fixtures/render docs/adr/0007-cdp-browser-opt-in.md
git commit -m "test(indexer): add ignored Obscura CDP compatibility probe and record results"
```

---

### Task 1: indexer — `Browser::render`、cookie 按 host 注入与真实回读、并发上限

**Files:**
- Modify: `crates/indexer/src/browser.rs`
- Modify: `crates/indexer/src/cdp_page.rs`
- Modify: `crates/indexer/src/cdp_page_transport.rs`（`Transport::connect` 的 scheme 校验）
- Modify: `crates/indexer/src/lib.rs:9`
- Test: `crates/indexer/tests/cdp_page.rs`, `crates/indexer/tests/browser_capabilities.rs`, `crates/indexer/tests/browser.rs`

**Interfaces:**
- Consumes: 既有 `PageSession`、`Transport::command`、`open_cdp_session`、`FetchRequest`。
- Produces:
  - `indexer::RenderedPage { pub html: String, pub cookie_header: Option<String> }`（`lib.rs` 导出）
  - `PageSession` 改形：
    ```rust
    pub trait PageSession: Send + Sync {
        fn goto(&self, url: &str) -> Result<(), IndexerError>;
        /// 按 host 绑定写入 cookie；实现必须用 Network.setCookies，不得用 setExtraHTTPHeaders。
        fn set_site_cookies(&self, page_url: &str, header: &str) -> Result<(), IndexerError>;
        fn content(&self) -> Result<String, IndexerError>;
        /// 默认返回 Err：不支持回读的实现不得伪造凭据。
        fn cookie_header(&self, page_url: &str) -> Result<String, IndexerError> {
            let _ = page_url;
            Err(IndexerError::Fetch("Browser cookie readback unsupported".into()))
        }
    }
    ```
    删除 `set_cookie_header`（及其 `Network.setExtraHTTPHeaders` 实现）。
  - `#[derive(Clone)] pub struct Browser`（`config`、`provider`、`opener` 已是 `Arc`/`Clone`，另加 `permits: Arc<Semaphore>`）
  - `Browser::render(&self, request: &FetchRequest) -> Result<RenderedPage, IndexerError>`
  - `Browser::fetch_html` 签名不变，内部 `self.render(request).map(|page| page.html)`
  - `Browser::with_max_sessions(self, n: usize) -> Self`（默认 2）；拿不到 permit → 立即 `Err(IndexerError::BrowserBusy)`，不阻塞（M3）
  - `IndexerError::BrowserBusy` 与 `IndexerError::BrowserSetup(String)` 变体：`open`、`set_site_cookies`、`BrowserBusy` 等**导航前**失败统一映射为 `BrowserSetup`（Task 7/8 据此决定是否允许回退）

- [ ] **Step 1: 写失败测试（cookie 注入方式）**

`crates/indexer/tests/cdp_page.rs` 的假 CDP 服务已记录收到的 method；新增：

```rust
#[test]
fn site_cookies_are_bound_to_the_site_host_not_sent_as_extra_headers() {
    // render(url = "http://127.0.0.1:{port}/torrents.php", cookie = "uid=1; pass=abc")
    // 断言：收到 Network.setCookies，params.cookies ==
    //   [{name:"uid",value:"1",url:"http://127.0.0.1:{port}/"},{name:"pass",value:"abc",url:...}]
    // 断言：从未收到 Network.setExtraHTTPHeaders
}
#[test]
fn https_site_cookies_are_marked_secure() {}
#[test]
fn session_cookies_are_cleared_when_the_session_ends() {
    // 会话 drop 前收到 Network.clearBrowserCookies（或 Target.disposeBrowserContext，按 Task 0 结论）
}
```

- [ ] **Step 2: 写失败测试（回读与去重）**

假服务 `Network.getCookies` 新增模式：校验请求 `params.urls == [当前页 URL]`，返回同名不同 path 的两条（`sid=old` path `/`、`sid=new` path `/forum`）以及 `uid=1`。

```rust
#[test]
fn cookie_readback_requests_cookies_for_the_current_page_url() {}
#[test]
fn cookie_readback_deduplicates_same_name_across_paths() {
    // 同名后出现者覆盖（与 crates/indexer/src/cdp.rs:219 name_map.insert 一致），再按 name 排序
    assert_eq!(page.cookie_header.as_deref(), Some("sid=new; uid=1"));
}
#[test]
fn readback_failure_yields_none_and_keeps_the_html() {
    // getCookies 返回 error → RenderedPage{ html: 正常, cookie_header: None }
}
#[test]
fn default_page_session_cookie_header_is_an_error_not_a_fake_session() {
    // 只实现必需方法的假 PageSession；断言 cookie_header(..).is_err()；
    // 并 grep 断言 browser.rs 不再含 "session=browser"
}
```

`crates/indexer/tests/browser.rs` 新增：

```rust
#[test] fn render_returns_html_and_read_back_cookie() {}
#[test] fn fetch_html_is_render_html() {}
#[test] fn target_is_closed_when_goto_fails() {
    // 假服务 Page.navigate 返回 errorText；断言随后收到 PUT /json/close/{id}
}
#[test] fn websocket_url_on_a_different_host_or_port_is_rejected() {
    // /json/new 返回 webSocketDebuggerUrl = ws://10.0.0.9:9/devtools/page/x → Err，且仍 close target
}
#[test] fn third_concurrent_render_is_busy_not_blocked() {
    // with_max_sessions(2)；opener 在 barrier 上阻塞两个会话；第三个立即 Err(BrowserBusy)
}
```

- [ ] **Step 3: 运行并确认失败**

Run: `cargo test -p indexer --test cdp_page --test browser --test browser_capabilities`
Expected: 编译失败（无 `render`/`RenderedPage`/`set_site_cookies`/`BrowserBusy`）。

- [ ] **Step 4: 实现 `cdp_page.rs`**

1. `set_site_cookies(page_url, header)`：
   - 用 `url` 解析 `page_url`，取 `origin = scheme://host[:port]/`；scheme 非 http/https 报错。
   - `header.split(';')` → `trim` → `split_once('=')`，跳过空 name；值原样保留（不解码）。
   - 发送 `Network.setCookies`，`params = {"cookies":[{"name","value","url":origin,"secure": scheme=="https","path":"/"}]}`。用 `url` 而不是 `domain`，让浏览器按 host-only 绑定，第三方主机拿不到。
   - 日志只记 `cookie_count`，**不记值**。
2. `cookie_header(page_url)`：
   ```rust
   let result = transport.command("Network.getCookies", json!({"urls":[page_url]}), deadline)?;
   let cookies = result["cookies"].as_array().ok_or_else(|| failure("getCookies", "missing cookies"))?;
   let mut by_name: BTreeMap<&str, &str> = BTreeMap::new();
   for c in cookies {
       if let (Some(n), Some(v)) = (c["name"].as_str(), c["value"].as_str()) {
           if !n.is_empty() { by_name.insert(n, v); }
       }
   }
   if by_name.is_empty() { return Err(failure("getCookies", "no cookies for page")); }
   Ok(by_name.iter().map(|(n, v)| format!("{n}={v}")).collect::<Vec<_>>().join("; "))
   ```
   （`BTreeMap` 同时完成去重与排序。）
3. `open_cdp_session`：取得 `webSocketDebuggerUrl` 后校验其 host 与 port 等于 discovery `base` 的 host 与 port（`ws`↔`http` 默认 80、`wss`↔`https` 默认 443），不等即 `Err`（H4）。此时 `session` 已构造，`Drop` 会 close target。
4. `Drop`：在 `transport.close()` 之前尽力发送 `Network.clearBrowserCookies`（失败只 `warn!`）；若 Task 0 证明支持 `Target.createBrowserContext`，改为 `/json/new` 之后 `Target.createTarget{browserContextId}` 并在 Drop 时 `Target.disposeBrowserContext`，测试按实际方案断言。

- [ ] **Step 5: 处理 HTTPS discovery 与 `wss`（H1）**

当前 `Transport::connect`（`cdp_page_transport.rs:39-44`）只接受 `ws`，而 `validate_endpoint` 接受 `https`。本计划选择**收窄**：`validate_endpoint` 只接受 `http`，错误文案改为「仅支持 http CDP discovery（建议仅回环或受信内网）」；ADR、`CONTEXT.md`、`response().capabilities.endpoint_schemes` 改为 `["http"]`。`wss` 支持（tungstenite `rustls-tls-webpki-roots` feature）列为后续工作。新增测试 `endpoint_validation_rejects_https_until_wss_is_supported`。

- [ ] **Step 6: 实现 `browser.rs`**

```rust
pub fn render(&self, request: &FetchRequest) -> Result<RenderedPage, IndexerError> {
    let _permit = self.permits.try_acquire().ok_or(IndexerError::BrowserBusy)?;
    let page = self.open(request.cdp_url.as_deref())?;   // 失败时尚无 target，无需清理
    let result = (|| {
        if let Some(cookie) = &request.cookie {
            page.set_site_cookies(&request.url, cookie)?;
        }
        page.goto(&request.url)?;
        let html = page.content()?;
        let cookie_header = page.cookie_header(&request.url)
            .inspect_err(|error| tracing::debug!(%error, key = %request.key, "Browser cookie readback unavailable"))
            .ok();
        Ok(RenderedPage { html, cookie_header })
    })();
    drop(page); // Arc 最后一个引用：触发 CdpPageSession::Drop → clear cookies + /json/close
    result.inspect_err(|error| tracing::error!(%error, request_key = %request.key, "Browser render failed"))
}
```
信号量用 `std::sync` 自写的 `try_acquire`（`Mutex<usize>` + RAII guard，约 20 行，放 `browser.rs` 底部），不引入 tokio。删除 `PageSession::set_cookie_header` 的所有实现与调用；`Browser` 加 `#[derive(Clone)]`。

- [ ] **Step 7: 导出**

`crates/indexer/src/lib.rs:9`：

```rust
pub use browser::{Browser, BrowserConfig, PageSession, RecordingFetcher, RenderedPage, RoutedFetcher};
```
`IndexerError` 增加 `#[error("Browser busy")] BrowserBusy`。

- [ ] **Step 8: 运行并确认通过**

Run: `cargo test -p indexer`
Expected: Step 1–2 与 Step 5 的新用例全部通过；既有 `cdp_page`、`browser`、`browser_capabilities`、`goldens` 用例不回归。

- [ ] **Step 9: 提交**

```bash
git add crates/indexer/src crates/indexer/tests
git commit -m "feat(indexer): bind site cookies per host, read real cookies back and expose Browser::render"
```

---

### Task 2: 单开关 — indexer 端点解析与回退 + api 设置模型与迁移

本 task 同时改 `indexer` 与 `api`，因为 `BrowserConfig` 换形后 `crates/api` 立刻编译失败；分两个提交会让中间提交处于红状态。

**Files:**
- Modify: `crates/indexer/src/browser.rs`, `crates/indexer/src/lib.rs`, `crates/indexer/src/engine.rs`（`SiteFailure.kind`）
- Modify: `crates/api/src/settings_keys.rs`, `crates/api/src/runtime_browser.rs`, `crates/api/src/http/settings.rs`（仅编译适配）
- Test: `crates/indexer/tests/browser_capabilities.rs`, `crates/indexer/tests/browser.rs`, `crates/api/tests/browser_runtime.rs`, `crates/api/tests/config_parsing.rs`, `crates/api/tests/management/browser_settings.rs`

**Interfaces:**
- Consumes: Task 1 的 `RenderedPage`、`Browser::render`、`Browser: Clone`。
- Produces:
  - **不新增 `CdpDisabled` 错误变体**（L1）：开关关闭是正常控制流，由 `Browser::try_render(&self, &FetchRequest) -> Result<Option<RenderedPage>, IndexerError>` 返回 `Ok(None)` 表达；`render` 保留为 `try_render` 的薄包装（`None` → `Err(Fetch("CDP disabled"))`），供 hooks 显式调用
  - `pub enum FetchSource { Http, Browser, HttpFallback { reason: String } }` 与 `Fetcher::fetch_traced(&self, &FetchRequest) -> Result<(String, FetchSource), IndexerError>`（默认实现 = `fetch` + `Http`；`RoutedFetcher` 覆盖）
  - `SiteFailure { site_id, error, kind: SiteFailureKind }`，`pub enum SiteFailureKind { Fetch, CdpFallbackEmpty }`（H5）
  - `pub enum BrowserRoute { Site, Instance, Managed }`
  - `pub struct BrowserConfig { pub enabled: bool, pub endpoint: Option<String>, pub managed_endpoint: Option<String> }`，`BrowserConfig::disabled()` 与 `Default` 等价
  - `BrowserConfig::validate_endpoint(endpoint: &str) -> Result<(), IndexerError>`
  - `BrowserConfig::resolve(&self, site: Option<&str>) -> Result<Option<(String, BrowserRoute)>, IndexerError>`；`Ok(None)` 表示开关关闭，调用方改用 HTTP
  - `Browser::cdp_enabled(&self) -> Result<bool, IndexerError>`（读 provider，供 API 层选择 transport）
  - `settings_keys::CDP_ENABLED = "cdp.enabled"`；删除 `CDP_SYNC_ENABLED`、`OBSCURA_ENABLED`、`OBSCURA_URL`；保留 `CDP_URL`
  - `BrowserSettings { cdp_enabled: bool, cdp_url: String, user_agent: String }`，方法 `load` / `cdp_enabled()` / `cdp_url()` / `user_agent()` / `config()` / `response()` / `save`

**`resolve` 的判定顺序（实现时逐条对齐）：**
1. `!enabled` → `Ok(None)`
2. `site` 为 `Some` → 校验；空/空白或非法即 `Err`（不得静默改走实例端点）→ `Some((site, Site))`
3. `endpoint` 为 `Some` 且非空白 → 校验 → `Some((ep, Instance))`
4. `managed_endpoint` 为 `Some` 且非空白 → 校验 → `Some((ep, Managed))`
5. 否则 `Err(Fetch("CDP 已开启但没有可用端点"))`

**`RoutedFetcher::fetch` 的新行为：**
- `!request.render` → 直接 `http.fetch`
- `render` 且 `try_render()` 返回 `Ok(None)` → `debug!` 后走 HTTP，`FetchSource::Http`（**不 warn**，这是用户选择）
- `render` 且返回 `Err`（含 `BrowserBusy`）→ `warn!`（含 `key`、`route`、`error`、`fallback="http"`）后走 HTTP，`FetchSource::HttpFallback{reason}`
- `Indexer::one`（`engine.rs`）改用 `fetch_traced`：来源为 `HttpFallback` 且解析出 0 个候选时，返回 `Err`，由 `run` 记为 `SiteFailure{kind: CdpFallbackEmpty, error: "CDP 回退 HTTP 后无结果: {reason}"}`；非 0 结果照常返回（降级成功不伪装成失败，但已 warn）。订阅与 UI 通过 `kind` 区分「站点降级」与「站点没有资源」。

**`BrowserSettings` 的遗留迁移（写回一次，L4 + M1）：**
- 新增 `pub fn migrate_legacy(store: &Store, site_endpoints_present: bool) -> Result<bool, StoreError>`，在 `seed_store_settings` 中于启动时调用一次，单事务：
  - `cdp.enabled` 已存在 → no-op。
  - 否则 `enabled = cdp.sync.enabled || obscura.enabled`（未知取值报错，不得静默关闭）；**若两者都关但任一 Site 有非空 `cdp_url`**（旧 ADR 允许「全局关 + Site 端点」）→ `enabled = true` 并 `info!(sites = n, "legacy per-Site CDP endpoints kept rendering; cdp.enabled=1")`，避免升级后静默失去渲染。
  - `cdp.url` 为空且 `obscura.url` 非空 → 写入 `cdp.url`。
  - 写入新 key 后删除 `cdp.sync.enabled`、`obscura.enabled`、`obscura.url`。
- `load()` 只读新 key；旧读取逻辑只存在于 `migrate_legacy`，计划在下一个 minor 版本删除 `migrate_legacy`（在 `CHANGELOG`/ADR 记录这个时间点）。
- `response()` 增加 `"ignored_site_endpoints": n`：开关关闭时有多少 Site 配置了 `cdp_url` 而被忽略，UI 显示告警。

**`response()` 形状：**
```json
{
  "cdp": {"enabled": true, "url": "", "configured": false, "route": "managed", "status": "unverified"},
  "user_agent": "crawler-media/0.1.0",
  "capabilities": {
    "mode": "cdp-opt-in", "managed_chromium": false, "managed_obscura": true,
    "endpoint_schemes": ["http"],
    "route_precedence": ["site", "instance", "managed"],
    "fallback": "http", "automatic_cloudflare_fallback": false
  }
}
```
`route`：`!enabled` → `"disabled"`；`url` 空白 → `"managed"`；否则 `"instance"`。`configured` = `enabled && !url.is_empty() && validate_endpoint(url).is_ok()`。

**不得回归的既有行为：** `save` 的多 key 写入失败回滚，以及 `crates/api/tests/browser_runtime.rs` 里用 SQLite trigger 制造写失败的测试，必须保持绿；换 key 后要确认该测试覆盖的是新的 key 集合。

- [ ] **Step 1: 写 indexer 侧的失败测试**

`crates/indexer/tests/browser_capabilities.rs` 新增（用注入 opener 记录实际端点，用记录型 HTTP fetcher 观察是否走 HTTP）：

```rust
#[test] fn switch_off_ignores_site_endpoint_and_uses_http() {}
#[test] fn switch_on_prefers_site_over_instance_over_managed() {}
#[test] fn switch_on_uses_instance_endpoint_when_site_is_absent() {}
#[test] fn switch_on_uses_managed_endpoint_when_nothing_is_configured() {}
#[test] fn switch_on_without_any_endpoint_falls_back_to_http() {}
#[test] fn cdp_failure_falls_back_to_http_fetch() {}
#[test] fn empty_site_endpoint_is_rejected_not_silently_rerouted() {}
#[test] fn endpoint_validation_rejects_credentials_query_and_ws() {}
#[test] fn switch_off_returns_none_not_an_error() {}
#[test] fn fallback_with_zero_results_is_a_visible_site_failure() {
    // render profile + 假 opener 失败 + HTTP 返回 JS 外壳 → outcome.failures[0].kind == CdpFallbackEmpty
}
#[test] fn fallback_with_results_reports_http_fallback_source() {}
```

- [ ] **Step 2: 运行并确认失败**

Run: `cargo test -p indexer --test browser_capabilities`
Expected: 编译失败（`BrowserConfig` 无 `resolve`，无 `try_render`/`FetchSource`）。

- [ ] **Step 3: 实现 `BrowserConfig` 新形状与 `resolve`，删除死代码**

删除 `enabled`（legacy managed）、`headless`、`obscura_enabled`、`obscura_url`、`with_cdp`、`with_obscura`、`validate_managed`、`enable_in`、`chromium_present`、`chromium_path`。`validate_endpoint` 改签名为 `&str`，只接受 `http`（见 Task 1 Step 5）。`lib.rs` 导出 `BrowserRoute`、`FetchSource`、`SiteFailureKind`。既有构造 `SiteFailure{..}` 的地方（`engine.rs:127` 与 api 侧测试）补 `kind: SiteFailureKind::Fetch`。

- [ ] **Step 4: 实现 `RoutedFetcher` 的回退，运行 indexer 测试**

Run: `cargo test -p indexer`
Expected: 全绿。

- [ ] **Step 5: 写 api 侧迁移测试**

`crates/api/tests/config_parsing.rs` 替换 `browser_settings_store_flag_uses_the_shared_vocabulary` 为：

```rust
#[test]
fn migration_maps_legacy_switches_once_and_deletes_them() {
    // 仅 cdp.sync.enabled=1 → cdp.enabled=1，旧 key 被删；再次运行 no-op
    // 仅 obscura.enabled=1 → cdp.enabled=1；cdp.enabled=maybe → load() 报错
}
#[test]
fn migration_keeps_rendering_for_legacy_site_endpoints() {
    // 两个旧开关都关 + 某 Site cdp_url 非空 → cdp.enabled=1
}
#[test]
fn legacy_obscura_url_is_adopted_as_the_instance_endpoint() {
    // 只有 obscura.url=http://127.0.0.1:9223 时，迁移后 cdp.url 为同一值，obscura.url 被删
}
#[test]
fn migration_never_overrides_an_explicit_switch() {}
```

`crates/api/tests/browser_runtime.rs` 更新为断言 `response()["capabilities"]["route_precedence"] == ["site","instance","managed"]` 与 `fallback == "http"`。

- [ ] **Step 6: 实现 `settings_keys`、`BrowserSettings`、`runtime_browser`**

同时把 `crates/api/src/http/settings.rs::sync_cdp_cookies` 的开关判断从 `CDP_SYNC_ENABLED` 改为 `CDP_ENABLED`（仅为编译与语义一致；端点解析的完整改造在 Task 9）。

- [ ] **Step 7: 运行 api 测试**

Run: `cargo test -p api --test config_parsing --test browser_runtime --test management`
Expected: 全绿。

- [ ] **Step 8: 全量检查后提交**

```bash
cargo check --workspace --tests
cargo test -p indexer && cargo test -p api
git add -A crates/indexer crates/api
git commit -m "feat: single CDP switch with site/instance/managed resolution and HTTP fallback"
```

---

### Task 3: 端点写入校验

**Files:**
- Modify: `crates/api/src/http/browser_settings.rs`, `crates/api/src/http/sites.rs:380-390`
- Test: `crates/api/tests/management/browser_settings.rs`, `crates/api/tests/management/site_credentials_review.rs`

**Interfaces:**
- Consumes: Task 2 的 `BrowserConfig::validate_endpoint`。
- Produces: `PUT /settings/browser` 与 `PUT/PATCH /sites/{id}` 在端点非法时返回 400 `browser.invalid_endpoint` / `site.invalid_endpoint`。

- [ ] **Step 1: 写失败测试**

```rust
#[tokio::test]
async fn browser_settings_reject_ws_credentials_and_query_endpoints() {
    // 三种 body 分别 PUT，断言 400 且 error.code == "browser.invalid_endpoint"
}
#[tokio::test]
async fn site_write_rejects_invalid_cdp_url() {
    // PUT /sites/{id} 带 cdp_url: "wss://user:pass@browser.example/cdp?token=x" → 400
}
```

- [ ] **Step 2: 运行并确认失败**

Run: `cargo test -p api --test management browser_settings_reject_ws_credentials_and_query_endpoints`
Expected: FAIL（当前返回 200）。

- [ ] **Step 3: 实现校验**

`BrowserSettings::save` 在 `apply` 后、写库前校验非空 `cdp_url`；`sites.rs` 的 `put_site`/`patch_site` 在写入 `cdp_url` 前调用 `BrowserConfig::validate_endpoint`。

- [ ] **Step 4: 运行并确认通过**

Run: `cargo test -p api --test management`
Expected: 全绿。

- [ ] **Step 5: 提交**

```bash
git add crates/api/src/http/browser_settings.rs crates/api/src/http/sites.rs crates/api/tests/management
git commit -m "feat(api): reject invalid CDP endpoints at write time"
```

---

### Task 4: ObscuraManager 生命周期 seam、哈希校验安装与进程身份

**Files:**
- Create: `crates/api/src/obscura_install.rs`
- Modify: `crates/api/src/obscura_manager.rs`, `crates/api/Cargo.toml`（`sha2` 已是依赖，无需新增）
- Test: `crates/api/tests/management/obscura_lifecycle.rs`, `crates/api/tests/management/obscura_install.rs`

**Interfaces:**
- Produces:
  - `pub trait ObscuraInstaller: Send + Sync { fn ensure_installed(&self, bin_dir: &Path) -> Result<PathBuf, InstallError>; }`
  - `pub trait DaemonHandle: Send + Sync { fn pid(&self) -> u32; fn try_alive(&self) -> bool; fn kill(&self); }`（`try_alive` 内部用 `Child::try_wait`）
  - `pub trait DaemonLauncher: Send + Sync { fn spawn(&self, binary: &Path, port: u16) -> Result<Box<dyn DaemonHandle>, String>; }`
  - `pub enum InstallPolicy { Download, PreinstalledOnly }`：设置 key `obscura.install = "download" | "preinstalled"`，env `CRAWLER_MEDIA_OBSCURA_INSTALL` 作启动默认；**默认 `preinstalled`**，需要 Admin 显式改为 `download` 才会联网下载
  - `ObscuraManager::{new, with_installer, with_launcher, with_ready_timeout, with_port_picker, binary_path, is_installed, is_downloading, owns_live_daemon, cdp_url, start, stop, shutdown, status}`
  - `cdp_url(&self) -> Option<String>`：**仅当本 manager 持有存活子进程且身份已确认时返回 `Some`**（H3）
  - `pub struct ObscuraStatus { installed, running, downloading, port, cdp_url, expected_version, reported_version, binary_path, install_policy, last_error }`（`Serialize`）
  - `obscura_install::{ReleaseInstaller, ProcessLauncher, OBSCURA_ASSETS, MAX_ARCHIVE_BYTES}`

**信任根（写进 ADR）：** 受管 Obscura 的信任根是代码中写死的 SHA-256，随 `OBSCURA_VERSION` 一起更新；我们信任的是「v0.2.3 这几个具体文件」，不是 `h4ckf0r0day/obscura` 仓库本身（该仓库自述为防检测浏览器，第三方维护，无上游签名）。升级版本必须人工下载、核对、改表、过 Task 0 spike。

```rust
pub const OBSCURA_VERSION: &str = "v0.2.3";
pub const MAX_ARCHIVE_BYTES: u64 = 200 * 1024 * 1024;
/// 2026-10-16 实测下载并计算；(os, arch, asset, sha256, size)
pub const OBSCURA_ASSETS: &[(&str, &str, &str, &str, u64)] = &[
    ("linux", "x86_64", "obscura-x86_64-linux.tar.gz",
     "1534d1e6ddaf3d080ec4091eb41d0a4d8cc042a48b607d3c410fc13b482a9eec", 70_880_637),
    ("linux", "aarch64", "obscura-aarch64-linux.tar.gz",
     "5ecf980bca3060236a7a86ec7ed83d943e6598ee87caa46d20325d90bc75f979", 68_921_993),
    ("macos", "x86_64", "obscura-x86_64-macos.tar.gz",
     "d7c48122debc2ad9b24842df44560860dba765ea928b3f636b7f053225245116", 68_697_908),
    ("macos", "aarch64", "obscura-aarch64-macos.tar.gz",
     "45653cfad226f1c9b415603a2ed59477fcbd6335c742338ce133c05de0bdd056", 66_569_973),
];
/// 归档内唯一允许的条目（实测 v0.2.3 归档恰好包含这两个文件）
pub const EXPECTED_ENTRIES: &[&str] = &["obscura", "obscura-worker"];
```

**`ReleaseInstaller::ensure_installed` 步骤（B1）：**
1. 已安装判定：`<data>/bin/obscura` 与 `obscura-worker` 存在，且 `<data>/bin/.obscura.sha256` 记录的归档哈希等于表中值 → 直接返回；预置二进制（`PreinstalledOnly`）则要求 `<data>/bin/obscura.tar.gz` 原始归档存在并过同样的哈希校验后解出，或二进制旁有与表一致的 `.obscura.sha256`（运维按 deploy 文档放置）。否则 `PreinstalledOnly` 返回 `InstallError::NotInstalled`，**不联网**。
2. `Download`：`ureq` agent 设置 `timeout_connect=10s`、`timeout_global=300s`；若响应 `Content-Length` > `MAX_ARCHIVE_BYTES` 或 != 表中 size → 拒绝。
3. 流式写入 `<data>/bin/.obscura-<nonce>.tar.gz.part`（`create_new`），边写边 `Sha256::update`，`Read::take(MAX_ARCHIVE_BYTES + 1)` 超限即拒绝。
4. 哈希或长度不符 → 删除 `.part`、`error!(expected, actual, asset)`、返回 `InstallError::HashMismatch`。
5. 校验通过后解压到 `<data>/bin/.obscura-<nonce>/` 临时目录：逐条遍历，只接受 `EXPECTED_ENTRIES` 中的普通文件（拒绝符号链接、硬链接、`..`、绝对路径、额外条目），单条目上限 `MAX_ARCHIVE_BYTES`。
6. `chmod 0755` 后对每个文件 `fs::rename` 到 `<data>/bin/`（同一文件系统，原子）；最后写 `.obscura.sha256`。任何一步失败清理临时目录与 `.part`。
7. 修复 L3：`is_running` 中的 `.parse().unwrap()` 改为 `SocketAddr::from((Ipv4Addr::LOCALHOST, port))`。

**`start()` / 身份确认（H3 + M4）：**
- 端口：每次启动由 `with_port_picker`（默认：绑定 `127.0.0.1:0` 取空闲端口后释放）选择，不再固定 9223。
- 启动参数：`serve --host 127.0.0.1 --port <port>`（`--host` 以 Task 0 结论为准；若不支持，ADR 记录默认绑定实测结果，非回环则拒绝启用受管模式）。
- 就绪：轮询 `GET http://127.0.0.1:<port>/json/version`（200ms 间隔，默认 10s 上限），且 `handle.try_alive()` 为真；`Browser` 字段写入 `reported_version`（L2：以 `/json/version` 为准，`OBSCURA_VERSION` 只是期望值，不一致 `warn!`）。
- 已有 `self.child` 且存活 → no-op；**端口上有服务但不是我们的子进程，绝不当作受管端点**。
- pidfile：启动成功后写 `<data>/run/obscura.pid`（`pid` + `port` + 启动 nonce）。`new()` 时读取旧 pidfile：若该 pid 存活且 `/proc/<pid>/exe`（macOS 用 `ps -o comm=`）指向 `<data>/bin/obscura` → `kill` 并 `info!("reaped orphaned managed Obscura")`；否则只删 pidfile，不杀任何进程。
- 子进程意外退出：`owns_live_daemon()` 用 `try_alive()`，状态 `running=false`、`last_error="exited"`，`cdp_url()` 返回 `None`。
- `shutdown()`：`stop()` + 删 pidfile；在 `main.rs` 的 graceful shutdown（axum `with_graceful_shutdown` 之后）调用。

- [ ] **Step 1: 写失败测试**

```rust
// obscura_install.rs（本地 TcpListener 假 HTTP 服务提供归档，不访问外网）
#[test] fn hash_mismatch_rejects_and_leaves_no_files() {}
#[test] fn oversized_download_is_rejected_before_writing_everything() {}
#[test] fn content_length_mismatch_is_rejected() {}
#[test] fn archive_with_unexpected_or_symlink_entries_is_rejected() {}
#[test] fn verified_archive_is_installed_atomically() {}
#[test] fn preinstalled_only_never_touches_the_network() {}
#[test] fn preinstalled_binary_without_matching_hash_is_not_installed() {}
// obscura_lifecycle.rs
#[test] fn start_installs_then_spawns_and_reports_running() {}
#[test] fn foreign_service_on_the_port_is_never_used_as_managed() {}
#[test] fn start_kills_the_child_and_errors_when_json_version_never_answers() {}
#[test] fn stop_only_kills_the_process_this_manager_spawned() {}
#[test] fn exited_child_reports_not_running_and_no_cdp_url() {}
#[test] fn stale_pidfile_for_a_foreign_process_is_removed_without_killing() {}
#[test] fn install_failure_is_reported_without_recording_a_child() {}
```
假 installer 返回临时文件路径；假 launcher 在 `spawn` 里起一个回答 `/json/version` 的 localhost 线程并由 handle 持有，或故意不回答以触发超时。测试用 `with_ready_timeout(Duration::from_millis(100))`，安装测试把 `MAX_ARCHIVE_BYTES`/资产表作为 `ReleaseInstaller` 的构造参数注入（生产用常量）。

- [ ] **Step 2: 运行并确认失败**

Run: `cargo test -p api --test management obscura`
Expected: 编译失败（无 `with_installer`、`InstallError` 等）。

- [ ] **Step 3: 实现 seam 与生产实现**

`obscura_manager.rs` 只保留 trait、manager、`status`、pidfile；下载/校验/解压与 `Command` 启动移到 `obscura_install.rs`（`#[path = "obscura_install.rs"] mod install;` 的既有仓库模式）。两个文件都需遵守 200 行软限制，超出则把归档校验拆到 `obscura_archive.rs`。`reconcile` 移到 Task 5（Job 化）。

- [ ] **Step 4: 运行并确认通过**

Run: `cargo test -p api --test management`
Expected: Step 1 各用例通过，既有用例不回归。

- [ ] **Step 5: 提交**

```bash
git add crates/api/src/obscura_manager.rs crates/api/src/obscura_install.rs crates/api/tests/management
git commit -m "feat(api): verify Obscura releases by pinned SHA-256 and own the daemon identity"
```

---

### Task 5: Obscura HTTP 路由与保存期 reconcile

**Files:**
- Create: `crates/api/src/http/obscura.rs`
- Modify: `crates/api/src/http/mod.rs`（`admin_ops_routes`，紧邻 `/settings/browser`）, `crates/api/src/http/browser_settings.rs`, `crates/api/src/management/state.rs`（新增 `with_obscura`）
- Test: `crates/api/tests/management/obscura_routes.rs`（新建，并在 `crates/api/tests/management/main.rs` 注册）

**Interfaces:**
- Consumes: Task 4 的 manager/status；既有 Job 体系（`JobKind`）。
- Produces:
  - `GET /api/v1/settings/browser/obscura` → `{installed, running, downloading, port, cdp_url, version, binary_path}`
  - `POST /api/v1/settings/browser/obscura/start` → 202 `{queued:true}`（结果经状态轮询）
  - `POST /api/v1/settings/browser/obscura/stop` → `{ok:true}`
  - `GET/PUT /settings/browser` 的响应新增 `"obscura"` 字段（进程事实，来自 manager），`cdp` 段保持 Task 2 形状
  - `ApiState::with_obscura(self, Arc<ObscuraManager>) -> Self`
  - `JobKind::BrowserReconcile`（M5）与 `pub fn reconcile(manager, want: ReconcileWant) -> Result<(), String>`
  - `pub struct ReconcileWant { switch_on: bool, instance_endpoint_configured: bool, render_sites: usize }`
  - `PUT /settings/browser` 的 body 可带 `"obscura": {"install": "download" | "preinstalled"}`

**按需启动（M2）：** 24 个内置 Profile 全部 `render: false`，因此只有 `switch_on && !instance_endpoint_configured && render_sites > 0` 才启动；`render_sites` = 启用中的 Site 中 Profile `render: true`（内置或 overlay，排除插件来源）的数量。否则 `stop()`。`response()` 增加 `"render_sites": n`，UI 显示「当前有 N 个站点需要浏览器」。Site 增删改与 overlay Profile 变更后也投递一次 `BrowserReconcile`。

- [ ] **Step 1: 写失败测试**

```rust
#[tokio::test] async fn obscura_routes_are_admin_only() {}          // member → 三个路由全 403
#[tokio::test] async fn obscura_status_reports_process_facts_not_intent() {}
#[tokio::test] async fn browser_settings_response_merges_obscura_status() {}
#[tokio::test] async fn saving_the_switch_on_without_an_endpoint_starts_the_managed_daemon() {}
#[tokio::test] async fn saving_the_switch_off_stops_the_managed_daemon() {}
#[tokio::test] async fn configuring_an_instance_endpoint_stops_the_managed_daemon() {}
#[tokio::test] async fn switch_on_without_any_render_site_does_not_start_the_daemon() {}
#[tokio::test] async fn saving_returns_before_the_install_finishes() {
    // 假 installer 阻塞在 barrier 上；PUT 立即 200，GET obscura → downloading=true
}
```

- [ ] **Step 2: 运行并确认失败**

Run: `cargo test -p api --test management obscura_routes`
Expected: 404 / 编译失败。

- [ ] **Step 3: 实现路由与保存期 reconcile**

保存只持久化配置，然后投递 `JobKind::BrowserReconcile`（去重：队列里已有同类 Job 则不再投递），**不在请求内下载或启动**，也不占用 `http_work` 并发额度。Job 在既有 job worker 线程执行（阻塞 IO 合法）；失败 `error!` 并写 `ObscuraStatus.last_error`，前端通过轮询 `GET /settings/browser/obscura` 的 `downloading` / `last_error` 观察。`POST .../start` 同样投递 Job 并返回 202 `{queued:true}`；`stop` 可同步执行（只杀子进程，耗时有界）。

- [ ] **Step 4: 运行并确认通过**

Run: `cargo test -p api --test management`
Expected: Step 1 各用例通过。

- [ ] **Step 5: 提交**

```bash
git add crates/api/src/http/obscura.rs crates/api/src/http/mod.rs crates/api/src/http/browser_settings.rs crates/api/src/management/state.rs crates/api/tests/management
git commit -m "feat(api): expose managed Obscura status/start/stop and reconcile on save"
```

---

### Task 6: 启动期与 provider 接线 — env 默认、移除 managed 拒绝、开机 reconcile、受管端点注入

启动期与请求期是同一份配置入口，且 `production_browser` 的签名只应改一次，因此合并为一个 task。

**Files:**
- Modify: `crates/api/src/runtime_browser.rs`, `crates/api/src/main.rs:92-115,186-206`, `crates/api/src/management/state.rs`
- Test: `crates/api/tests/browser_runtime.rs`, `crates/api/tests/config_parsing.rs`

**Interfaces:**
- Consumes: Task 2 的 `BrowserSettings`/`BrowserConfig`、Task 4 的 `reconcile`/`ObscuraManager`。
- Produces:
  - `pub fn seed_switch_default(store: &Store, browser_enabled: bool) -> Result<(), StoreError>`：仅当 `cdp.enabled` 不存在且 `browser_enabled` 为真时写入 `"1"`
  - `pub fn production_browser(store: Arc<Mutex<Store>>, obscura: Arc<ObscuraManager>) -> Result<Browser, IndexerError>`（**去掉 `managed` 参数**）
  - `pub fn boot_reconcile(store: &Store, manager: &ObscuraManager) -> Result<(), String>`
  - `ApiState.browser: Browser`（`pub(crate)`）与 `with_browser(self, Browser) -> Self`
  - provider closure 每次读取设置后返回：`enabled = cdp.enabled`；`endpoint = cdp.url` 非空时 `Some`；`managed_endpoint = if enabled && url 为空 { manager.cdp_url() } else { None }`——`cdp_url()` 只在本 manager 持有存活且身份已确认的子进程时为 `Some`（读内存状态 + `try_wait`，无 TCP 探测）

**行为约束：**
- `CRAWLER_MEDIA_BROWSER=1` 不再拒绝启动，行为等同打开开关（经 `seed_switch_default` 落成显式设置）。用户已显式设置过 `cdp.enabled` 时，env 不得覆盖。
- 受管端点**不做**每请求 TCP 探活；只读 manager 的内存状态（H3）。端口被外部进程占用时 `cdp_url()` 为 `None`，按 Task 2 的「无可用端点」回退 HTTP 并 warn。
- `boot_reconcile` 只投递 `JobKind::BrowserReconcile`，不在启动路径上下载。
- `main.rs` shutdown 时调用 `manager.shutdown()`（M4）。
- deploy 文档注明：`network_mode: host` 下受管 Obscura 仍只绑定 `127.0.0.1`，但同宿主的其他进程可访问该端口；需要更强隔离时把 Obscura 放进独立容器/netns（后续工作）。
- `boot_reconcile` 失败只 `error!`，不阻止服务启动。

- [ ] **Step 1: 写失败测试**

```rust
#[test] fn env_flag_seeds_the_switch_only_when_it_is_unset() {}
#[test] fn env_flag_never_overrides_an_explicit_switch_value() {}
#[test] fn production_browser_no_longer_rejects_the_legacy_managed_flag() {}
#[tokio::test] async fn switch_on_without_an_instance_endpoint_passes_the_managed_endpoint() {}
#[tokio::test] async fn managed_endpoint_is_absent_when_the_manager_owns_no_live_daemon() {}
#[tokio::test] async fn switch_on_with_an_instance_endpoint_ignores_the_managed_one() {}
#[tokio::test] async fn switch_off_never_opens_a_browser_session() {}
#[tokio::test] async fn switch_off_ignores_a_site_level_endpoint() {}
#[tokio::test] async fn saving_settings_changes_the_next_request_without_restart() {}
```
删除 `browser_runtime.rs` 中 `production_browser(store.clone(), true).is_err()` 的断言。

- [ ] **Step 2: 运行并确认失败**

Run: `cargo check --workspace --all-targets && cargo test -p api --test browser_runtime`
Expected: 编译失败（参数个数不符）；记录所有报错的调用方（含 `crates/api/src/bin/live_loop`、`fs_watcher_tests`），Step 3 一并修改。

- [ ] **Step 3: 实现并接线 main.rs**

`seed_store_settings` 调用 `seed_switch_default`；`create_api_state` 在 state 构建后调用 `boot_reconcile`；`production_browser(store.clone(), obscura.clone())` 构建一次，`clone()` 一份进 `ApiState`（Task 8 用），另一份进 `RoutedFetcher`。

- [ ] **Step 4: 运行并确认通过**

Run: `cargo test -p api --test browser_runtime --test config_parsing`
Expected: 全绿。

- [ ] **Step 5: 提交**

```bash
git add crates/api/src/runtime_browser.rs crates/api/src/main.rs crates/api/src/management/state.rs crates/api/tests
git commit -m "feat(api): make CRAWLER_MEDIA_BROWSER the boot default and inject the managed endpoint"
```

---

### Task 7: hooks — 浏览器登录/签到的 transport 与真实 cookie 持久化

**Files:**
- Modify: `crates/hooks/src/plugins.rs`
- Test: `crates/hooks/tests/login_checkin.rs`, `crates/hooks/tests/review_remediation_login.rs`

**Interfaces:**
- Consumes: Task 1 的 `Browser::render` / `RenderedPage`。
- Produces:
  - `CheckInPlugin::browser(bus, store, browser: &'a Browser) -> Self`（与既有 `http` / `new` 并列）
  - 内部统一取页结果：`struct FetchedPage { body: String, cookie_header: Option<String> }`
  - 登录与签到**都**在业务判定成功后合并持久化 cookie：`merge_cookie_string(site.cookie.as_deref(), &header)`；`cookie_header` 为 `None` 时保留原 cookie，**不写空值也不合成占位值**

**行为约束：**
- 浏览器签到默认用导航（GET）访问 `{site_base}attendance.php`；若 Task 0 证明 GET 不真实签到（L7），改为先导航 `index.php`，再 `Runtime.evaluate` 点击签到链接/提交签到表单，并在 ADR 中按站点记录。HTTP 路径保持 POST 不变。
- 插件返回的错误区分阶段（H5）：`PluginError::BrowserPreNavigation(String)`（端点解析、`BrowserBusy`、`/json/new`、WebSocket、`setCookies` 失败——服务端未收到请求）与 `PluginError::Fetch`（已发出导航之后）。实现方式：`Browser::render` 的错误用 `IndexerError::BrowserSetup(String)` 标识导航前失败（Task 1 的 `open`/`set_site_cookies`/`BrowserBusy` 映射到它），hooks 原样映射。
- `cookie` 只经 `Network.setCookies` 注入（Task 1），不会发往第三方主机。
- 两条路径共用同一 `SiteResponsePolicy`（`NexusPhpPolicy`），transport 不影响判定。

- [ ] **Step 1: 写失败测试**

```rust
#[test] fn browser_check_in_verifies_the_business_outcome_from_the_rendered_page() {}
#[test] fn browser_check_in_persists_the_read_back_cookie() {}
#[test] fn browser_login_persists_the_read_back_cookie() {}
#[test] fn browser_page_without_a_read_back_cookie_keeps_the_existing_cookie() {}
#[test] fn browser_check_in_still_rejects_a_challenge_page() {}
#[test] fn already_checked_in_page_is_a_success_on_both_transports() {
    // body 含「您今天已签到」/"You have already attended"（site_policy.rs:77-83）→ Ok
}
#[test] fn setup_failure_is_reported_as_pre_navigation() {}
```

- [ ] **Step 2: 运行并确认失败**

Run: `cargo test -p hooks`
Expected: 编译失败（无 `CheckInPlugin::browser`）。

- [ ] **Step 3: 实现**

`browser_page` 改为返回 `FetchedPage`；`check_in_inner` 与 `login_inner` 统一走「取页 → `reject_challenge` → policy → 持久化 cookie」。保持 `require_cookie` 前置不变（浏览器路径同样要求已有 cookie，不提交凭据）。

- [ ] **Step 4: 运行并确认通过**

Run: `cargo test -p hooks`
Expected: 全绿。

- [ ] **Step 5: 提交**

```bash
git add crates/hooks/src/plugins.rs crates/hooks/tests
git commit -m "feat(hooks): run login and check-in on the Browser and persist real read-back cookies"
```

---

### Task 8: api — 登录/签到的 via 选择与 browser→HTTP 回退

**Files:**
- Create: `crates/api/src/site_maintenance.rs`
- Modify: `crates/api/src/lib.rs`（`mod site_maintenance;`）, `crates/api/src/http/sites.rs:460-545`, `crates/api/src/check_in.rs:23-58`
- Test: `crates/api/tests/management/site_maintenance.rs`（新建并注册）

**Interfaces:**
- Consumes: Task 6 的 `ApiState.browser`、Task 7 的 `CheckInPlugin::browser`、既有 `site_policy`。
- Produces:
  - `pub(crate) fn login(state: &ApiState, site: &Site) -> Result<(), PluginError>`
  - `pub(crate) fn check_in(state: &ApiState, site: &Site) -> Result<(), PluginError>`

**判定与回退规则：**
1. `policy = site_policy(state, site)?`（非 NexusPHP / 无 profile 仍按原样报错）
2. `use_browser = profile.render && state.browser.cdp_enabled()?`——读取失败（非法设置值）**不得静默当作关闭**（M7）：`error!` 并把错误返回给调用方
3. `use_browser` → 先跑 browser 插件：
   - `Err(PluginError::BrowserPreNavigation(e))` → `warn!`（含 `site_id`、`error`、`fallback = "http"`）后跑 HTTP 插件
   - 其它 `Err`（导航已发出）→ **不回退**，直接返回错误（避免重复签到/重复登录请求，H5）
4. 否则直接跑 HTTP 插件

`site_login` / `site_check_in` / `check_in::run` 三处都改为调用这两个函数，删除各自内联的插件构造。

- [ ] **Step 1: 写失败测试**

```rust
#[tokio::test] async fn render_profile_without_cdp_uses_http_login() {}
#[tokio::test] async fn browser_login_setup_failure_falls_back_to_http_and_succeeds() {}
#[tokio::test] async fn browser_check_in_setup_failure_falls_back_to_http() {}
#[tokio::test] async fn browser_check_in_failure_after_navigation_does_not_post_again() {
    // 假 Browser 在 content() 超时；断言 HTTP post 计数 == 0，返回 Err
}
#[tokio::test] async fn invalid_switch_value_is_an_error_not_http() {}
#[tokio::test] async fn non_render_profile_never_attempts_the_browser() {}
#[tokio::test] async fn http_fallback_failure_is_reported_as_a_failure_not_a_success() {}
```

- [ ] **Step 2: 运行并确认失败**

Run: `cargo test -p api --test management site_maintenance`
Expected: 编译失败（无 `site_maintenance`）。

- [ ] **Step 3: 实现并替换三处调用点**

- [ ] **Step 4: 运行并确认通过**

Run: `cargo test -p api --test management && cargo test -p hooks`
Expected: 全绿。

- [ ] **Step 5: 提交**

```bash
git add crates/api/src/site_maintenance.rs crates/api/src/lib.rs crates/api/src/http/sites.rs crates/api/src/check_in.rs crates/api/tests/management
git commit -m "feat(api): choose the Browser transport for render Profiles and fall back to HTTP on failure"
```

---

### Task 9: `sync-cdp` 走新开关与解析后的端点

**Files:**
- Modify: `crates/api/src/http/settings.rs:225-260`
- Test: `crates/api/tests/management/browser_settings.rs`

**Interfaces:**
- Consumes: Task 2 的 `CDP_ENABLED`/`CDP_URL`、Task 4 的 `ObscuraManager::cdp_url`。
- Produces：`POST /settings/browser/sync-cdp` 的端点来源 = 非空 `cdp.url`，否则受管 Obscura 的 `cdp_url()`；开关关闭 → 400 `cdp.disabled`；受管 Obscura 的 `cdp_url()` 为 `None`（未运行或非本进程拉起）且无实例端点 → 400 `cdp.no_endpoint`。

- [ ] **Step 1: 写失败测试**

```rust
#[tokio::test] async fn sync_cdp_requires_the_switch_not_the_legacy_key() {}
#[tokio::test] async fn sync_cdp_uses_the_managed_obscura_endpoint_when_no_instance_url_is_set() {}
#[tokio::test] async fn sync_cdp_reports_no_endpoint_instead_of_guessing() {}
```

- [ ] **Step 2: 运行并确认失败**

Run: `cargo test -p api --test management sync_cdp`
Expected: FAIL。

- [ ] **Step 3: 实现端点解析**

- [ ] **Step 4: 运行并确认通过**

Run: `cargo test -p api`
Expected: 全绿。

- [ ] **Step 5: 提交**

```bash
git add crates/api/src/http/settings.rs crates/api/tests/management/browser_settings.rs
git commit -m "feat(api): resolve the cookie-sync endpoint from the single switch"
```

---

### Task 10: Web UI — 单开关与受管 Obscura 状态/控制

**Files:**
- Modify: `web/components/site-config-section.tsx:1293-1530`
- Create: `web/lib/browser-settings.ts`（纯函数：`buildBrowserSettingsBody`、`describeObscuraStatus`）
- Test: `web/test/cdp-browser-settings.test.mjs`（新建）

**Interfaces:**
- Consumes: Task 2 的 `GET/PUT /settings/browser` 形状、Task 5 的三个 Obscura 路由。
- Produces：单一开关 + 可选端点输入 + 受管 Obscura 状态区（installed/running/version/port + 安装并启动 / 停止 按钮）+ 保留 cookie 同步按钮；文案必须说明「关闭时不使用 CDP」「CDP 失败会回退 HTTP」。

**测试模式（L5）：** 不再用源码正则断言组件文本。把请求体构造与状态文案抽成 `web/lib/browser-settings.ts` 的纯函数，用 `node --test` 直接调用（参照 `web/test/` 中已有对 `lib/` 的单测写法；若需 TS 转译，沿用仓库现有方式）。

- [ ] **Step 1: 写失败测试**

```javascript
test("body carries one switch and no legacy obscura toggle", () => {
  assert.deepEqual(buildBrowserSettingsBody({ enabled: true, url: "", install: "preinstalled" }),
    { cdp: { enabled: true, url: "" }, obscura: { install: "preinstalled" } });
});
test("status text reflects process facts, render site count and ignored endpoints", () => {
  assert.match(describeObscuraStatus({ running: false, downloading: true, render_sites: 2 }), /下载中/);
  assert.match(describeObscuraStatus({ running: false, render_sites: 0 }), /没有站点需要浏览器/);
});
```

- [ ] **Step 2: 运行并确认失败**

Run: `cd web && node --test test/cdp-browser-settings.test.mjs`
Expected: FAIL。

- [ ] **Step 3: 改造面板**

删除独立的 Obscura 开关与 URL 输入；`cdp.url` 留空即表示使用受管 Obscura，UI 必须显式提示这一点。新增安装策略选择（仅预置 / 允许自动下载，默认仅预置）、`render_sites`、`ignored_site_endpoints` 告警与 `last_error` 显示；start 返回 202 后轮询状态。

- [ ] **Step 4: 运行并确认通过**

Run: `cd web && node --test && pnpm typecheck`
Expected: 全部通过。

- [ ] **Step 5: 提交**

```bash
git add web/components/site-config-section.tsx web/lib/browser-settings.ts web/test/cdp-browser-settings.test.mjs
git commit -m "feat(web): one CDP switch with managed Obscura status and controls"
```

---

## 与插件方案的对接（M6 / H4，本计划只写约束，接入在插件方案落地时做）

- CDP discovery / close（`cdp_page.rs` 的 `http_agent`）与 `connect_tcp` 将来改走 net-guard 的「浏览器实例策略」（与下载器实例同级，由宿主配置允许的 host:port）；在此之前，Task 1 的 `webSocketDebuggerUrl` host:port 一致性校验与「端点只接受 http」是最低防线。
- 浏览器自身的网络栈（页面 JS、子资源、跳转）不经过 net-guard，因此 **render 只对内置与 Admin 本地 overlay Profile 开放**；插件来源 Profile 设置 `render: true` 在加载时被拒绝（`crates/indexer/src/profile.rs` 加载入口加来源参数，Task 2 内实现并测试 `plugin_profile_cannot_enable_render`）。
- exec-ctx 在插件方案 M0a 落地后，`Browser::render` 入口加 `debug_assert!(exec_ctx::is_blocking_ok())`；Task 1 的 Browser 信号量纳入插件方案的并发预算体系。单次 render 总时限对齐 `blocking_budget`（由调用方传入 deadline，后续工作）。
- 把受管 Obscura 放进受限网络（容器 / netns）列为后续工作。

## 收尾

- [ ] `cargo check --workspace --all-targets` 与 `cargo test --workspace` 全绿。
- [ ] `cd web && node --test && pnpm typecheck` 全绿。
- [ ] **手动验收（有网络的机器，结果写进 ADR）**：设 `obscura.install=download` → 开关开启 → 下载并哈希校验 → 启动（确认仅监听 127.0.0.1、端口随机）→ 配一个 `render: true` 的 overlay Profile 指向本地 JS 渲染页 → 搜索得到结果 → 浏览器签到 → cookie 回读并合并 → 关闭开关后进程退出、pidfile 删除。
- [ ] 按 Review Focus 九条逐条确认对应测试存在且真的在断言该行为。
- [ ] ADR-0007 与 `docs/deploy.md` 补充：信任根是写死的 SHA-256（Task 4）；离线部署的预置路径 `<data>/bin/obscura`、`<data>/bin/obscura-worker` 与 `<data>/bin/.obscura.sha256`（或放原始归档 `<data>/bin/obscura.tar.gz`），预置同样过哈希校验（L6）；`obscura.install` 默认 `preinstalled`；`network_mode: host` 风险；端点仅 `http`。
- [ ] 确认 `docs/adr/0007-cdp-browser-opt-in.md`、`CONTEXT.md`、`docs/deploy.md` 的描述与最终实现一致；如有偏差，改代码或改文档，不允许两边不一致。
- [ ] 在 worktree 提交完整分支，`main` 保持干净。
