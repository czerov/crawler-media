# CDP/Obscura 受管浏览器链路 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让实例级单一 CDP 开关成立：开启时按 Site `cdp_url` → 实例 `cdp.url` → 受管 Obscura 解析端点，任一 CDP 环节失败回退 HTTP；关闭时完全不使用 CDP；登录与签到可走浏览器并持久化真实回读的 cookie。

**Architecture:** 路由决策全部下沉到 `crates/indexer` 的 `BrowserConfig::resolve` 与 `RoutedFetcher`，Indexer 仍不依赖 API/Store；`crates/api` 通过 provider closure 在每次请求读取实时设置，并把受管 Obscura 的本地端点作为第三级兜底注入。登录/签到的 transport 选择与失败回退由 `crates/api` 的新模块统一编排，业务判定复用 `crates/hooks` 既有的 `SiteResponsePolicy`，因此 transport 不影响「签到成功」的结论。

**Tech Stack:** Rust 2024 workspace（`indexer` / `hooks` / `api`）、axum、rusqlite、tungstenite、thiserror、tracing；前端 Vite + React + `node --test`。

**Spec:** `docs/adr/0007-cdp-browser-opt-in.md`（决策）、`CONTEXT.md` 的 **Browser** 词条、`docs/deploy.md` 的部署边界。执行前先读这三处。

## 起点

Task 1 的行为已有验证过的原型实现：`cargo test -p indexer` = 43 passed / 0 failed（lib 2、browser 5、browser_capabilities 13、cdp_match 2、cdp_page 10、goldens 11），存档于 `/tmp/indexer-cookie-readback-verified.patch`。若该文件存在，先 `git apply` 再按 Task 1 的 Steps 补两处原型缺口（`lib.rs` 导出、按 name 去重）。原型不含其余任何任务。

## Global Constraints

- 工作目录：`.worktrees/cdp-obscura`，分支 `feat/cdp-obscura`。所有改动在此 worktree 内，`main` 保持干净。
- 每个被触及的 crate 先 `cargo test -p <crate>`；全部完成后跑一次 `cargo test --workspace`。
- 测试不得访问实网、不得启动真实 Chromium/Obscura。CDP 用 localhost 假服务，安装/启动/HTTP 用注入 trait。
- `indexer` 不得依赖 `api`/`store`；`hooks` 不得依赖 `api`；`domain` 仅类型无 IO。
- 尺寸硬限制：`.rs` 文件 800 行（软 200）、生产函数 120 行（软 60）、测试函数 120 行（软 80）、`lib.rs` 软 150。超软限需在 PR 描述里说明理由。
- 无 `unwrap`/`expect` 于 IO 或解析路径（测试除外）；错误用 `thiserror`。
- 结构化日志：`error!` 不可预期失败；`warn!` 可恢复降级——**本次所有 CDP 回退必须 warn**；`info!` 生命周期（安装/启动/停止/登录/签到）；`debug!` 细节。**cookie 明文绝不进日志**。
- 术语遵循 `CONTEXT.md`：**Browser**、**Check-in**、**Site**、**Indexer**、**Profile**、**Torrent**、**Plugin**。
- 每个 task 结束时提交一次，`feat:` / `refactor:` / `test:` 前缀。不提交无关文件。

## Review Focus

以下 6 类输入/失败模式最容易伤人，且没有现成测试覆盖。每条都已在对应 task 里安排了测试，实现时不要跳过。

1. **开关关闭但 Site 配了 `cdp_url`**：用户期望「没开 CDP 就完全不用 CDP」。这是本次相对旧实现的翻转点，必须证明 HTTP 被使用且 opener 从未被调用。
2. **CDP 失败回退 HTTP 被伪装成成功**：回退必须可观测（warn 含 Site/route/原因），且结果来源必须真实——不能因为「回退过了」就把 HTTP 的空结果当成渲染成功。
3. **`Network.getCookies` 返回同名多条**（不同 path/domain）：拼出的 header 必须按 name 去重，否则是非法 `name=v1; name=v2`。
4. **受管 Obscura 未就绪**（未安装/下载失败/端口被外部服务占用）：不得杀外部进程，且请求必须回退 HTTP 而不是挂死。
5. **浏览器路径与 HTTP 路径的登录/签到判定必须同源**：同一个 `SiteResponsePolicy`，否则「签到成功」会随 transport 变化。
6. **端点非法**（`ws://`、内嵌凭据、带 query）：不得静默改走别的端点或悄悄降级——写入时必须 400 拒绝。

---

### Task 1: indexer — CDP cookie 真实回读与 `Browser::render`

**Files:**
- Modify: `crates/indexer/src/browser.rs`
- Modify: `crates/indexer/src/cdp_page.rs`
- Modify: `crates/indexer/src/lib.rs:9`
- Test: `crates/indexer/tests/cdp_page.rs`, `crates/indexer/tests/browser_capabilities.rs`

**Interfaces:**
- Consumes: 既有 `PageSession`、`crates/indexer/src/cdp_page_transport.rs` 的 `Transport::command`。
- Produces:
  - `indexer::RenderedPage { pub html: String, pub cookie_header: Option<String> }`（需从 `lib.rs` 导出）
  - `Browser::render(&self, request: &FetchRequest) -> Result<RenderedPage, IndexerError>`
  - `Browser: Clone`
  - `PageSession::cookie_header(&self) -> Result<String, IndexerError>`，默认实现返回 `Err`，**不得返回任何硬编码字符串**
  - `Browser::fetch_html` 签名与语义不变，内部改为 `self.render(request).map(|page| page.html)`

- [ ] **Step 1: 应用原型补丁（若存在）**

```bash
test -f /tmp/indexer-cookie-readback-verified.patch && git apply /tmp/indexer-cookie-readback-verified.patch
cargo test -p indexer
```
Expected: 43 passed / 0 failed。

- [ ] **Step 2: 补 `lib.rs` 导出**

`crates/indexer/src/lib.rs:9` 增加 `RenderedPage`：

```rust
pub use browser::{Browser, BrowserConfig, PageSession, RecordingFetcher, RoutedFetcher, RenderedPage};
```

- [ ] **Step 3: 写按 name 去重的失败测试**

在 `crates/indexer/tests/cdp_page.rs` 的假 CDP 服务上新增一个模式，让 `Network.getCookies` 对同一 name 返回不同 path 的两条（例如 `sid=old` path `/`、`sid=new` path `/forum`）。

```rust
#[test]
fn cookie_readback_deduplicates_same_name_across_paths() {
    // 期望：只有一个 sid，且取后出现的那条（与 cdp.rs 的 name_map 语义一致）
    assert_eq!(header, "sid=new");
}
```

- [ ] **Step 4: 运行并确认失败**

Run: `cargo test -p indexer --test cdp_page cookie_readback_deduplicates_same_name_across_paths`
Expected: FAIL，header 为 `sid=old; sid=new`。

- [ ] **Step 5: 实现去重**

`CdpPageSession::cookie_header` 在拼接前按 name 去重（同名后写覆盖先写，与 `crates/indexer/src/cdp.rs` 的 `name_map.insert` 一致），再按 `"name=value"` 排序。

- [ ] **Step 6: 运行并确认通过**

Run: `cargo test -p indexer`
Expected: 全绿（44 passed）。

- [ ] **Step 7: 提交**

```bash
git add crates/indexer/src/browser.rs crates/indexer/src/cdp_page.rs crates/indexer/src/lib.rs crates/indexer/tests/cdp_page.rs crates/indexer/tests/browser_capabilities.rs
git commit -m "feat(indexer): read real session cookies back from CDP and expose Browser::render"
```

---

### Task 2: 单开关 — indexer 端点解析与回退 + api 设置模型与迁移

本 task 同时改 `indexer` 与 `api`，因为 `BrowserConfig` 换形后 `crates/api` 立刻编译失败；分两个提交会让中间提交处于红状态。

**Files:**
- Modify: `crates/indexer/src/browser.rs`, `crates/indexer/src/lib.rs`（新增 `CdpDisabled` 变体）
- Modify: `crates/api/src/settings_keys.rs`, `crates/api/src/runtime_browser.rs`, `crates/api/src/http/settings.rs`（仅编译适配）
- Test: `crates/indexer/tests/browser_capabilities.rs`, `crates/indexer/tests/browser.rs`, `crates/api/tests/browser_runtime.rs`, `crates/api/tests/config_parsing.rs`, `crates/api/tests/management/browser_settings.rs`

**Interfaces:**
- Consumes: Task 1 的 `RenderedPage`、`Browser::render`、`Browser: Clone`。
- Produces:
  - `IndexerError::CdpDisabled`（控制信号，不是给用户看的失败）
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
- `render` 且 `render()` 返回 `Err(CdpDisabled)` → `debug!` 后走 HTTP（**不 warn**，这是用户选择）
- `render` 且 `render()` 返回其它 `Err` → `warn!`（含 `key`、`error`）后走 HTTP

**`BrowserSettings` 的遗留读取（一次性兼容，不写回）：**
- `cdp.enabled` 存在 → 用它；否则 `cdp.sync.enabled`；否则 `obscura.enabled`；都没有 → false
- `cdp.url` 非空 → 用它；否则 `obscura.url` 非空 → 用它；否则 `""`
- 未知取值（如 `"maybe"`）仍然报错，不得静默关闭

**`response()` 形状：**
```json
{
  "cdp": {"enabled": true, "url": "", "configured": false, "route": "managed", "status": "unverified"},
  "user_agent": "crawler-media/0.1.0",
  "capabilities": {
    "mode": "cdp-opt-in", "managed_chromium": false, "managed_obscura": true,
    "endpoint_schemes": ["http", "https"],
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
```

- [ ] **Step 2: 运行并确认失败**

Run: `cargo test -p indexer --test browser_capabilities`
Expected: 编译失败（`BrowserConfig` 无 `resolve`）。

- [ ] **Step 3: 实现 `BrowserConfig` 新形状与 `resolve`，删除死代码**

删除 `enabled`（legacy managed）、`headless`、`obscura_enabled`、`obscura_url`、`with_cdp`、`with_obscura`、`validate_managed`、`enable_in`、`chromium_present`、`chromium_path`。`lib.rs` 增加 `CdpDisabled` 变体与 `BrowserRoute` 导出。

- [ ] **Step 4: 实现 `RoutedFetcher` 的回退，运行 indexer 测试**

Run: `cargo test -p indexer`
Expected: 全绿。

- [ ] **Step 5: 写 api 侧迁移测试**

`crates/api/tests/config_parsing.rs` 替换 `browser_settings_store_flag_uses_the_shared_vocabulary` 为：

```rust
#[test]
fn cdp_switch_reads_new_key_then_legacy_keys() {
    // cdp.enabled=on → true；仅 cdp.sync.enabled=1 → true；仅 obscura.enabled=1 → true；
    // cdp.enabled=maybe → load() 报错
}
#[test]
fn legacy_obscura_url_is_adopted_as_the_instance_endpoint() {
    // 只有 obscura.url=http://127.0.0.1:9223 时，cdp.url 读出同一值
}
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

### Task 4: ObscuraManager 生命周期 seam

**Files:**
- Create: `crates/api/src/obscura_install.rs`
- Modify: `crates/api/src/obscura_manager.rs`
- Test: `crates/api/tests/management/obscura_lifecycle.rs`

**Interfaces:**
- Produces:
  - `pub trait ObscuraInstaller: Send + Sync { fn ensure_installed(&self) -> Result<(), String>; }`
  - `pub trait DaemonHandle: Send + Sync { fn kill(&self); }`
  - `pub trait DaemonLauncher: Send + Sync { fn spawn(&self, binary: &Path, port: u16) -> Result<Box<dyn DaemonHandle>, String>; }`
  - `ObscuraManager::{new, with_port, with_installer, with_launcher, with_ready_timeout, binary_path, is_installed, is_downloading, is_running, port, cdp_url, start, stop, status}`
  - `pub struct ObscuraStatus { installed, running, downloading, port, cdp_url, version, binary_path }`（`Serialize`）
  - `pub fn reconcile(manager: &ObscuraManager, switch_on: bool, instance_endpoint_configured: bool) -> Result<(), String>`
  - `obscura_install::{ReleaseInstaller, ProcessLauncher}`（生产实现：GitHub Releases 下载解压 + `Command` 启动）

**行为约束：**
- `start()`：已在运行 → info 后 `Ok`（**不重复拉起**）；未安装 → 安装；`spawn` 后等待端口就绪（默认 5s，测试可缩短）；超时 → 杀掉刚拉起的进程并返回 `Err`。
- `stop()`：只杀 `self.child` 里本进程拉起的进程；没有则 `debug!` 后 no-op（**外部 daemon 不受影响**）。
- `reconcile`：`switch_on && !instance_endpoint_configured` → `start()`；否则 `stop()`。

- [ ] **Step 1: 写失败测试**

```rust
#[test] fn start_installs_then_spawns_and_reports_running() {}
#[test] fn start_is_a_noop_when_the_port_is_already_serving() {}
#[test] fn start_kills_the_child_and_errors_when_the_port_never_answers() {}
#[test] fn stop_only_kills_the_process_this_manager_spawned() {}
#[test] fn install_failure_is_reported_without_recording_a_child() {}
#[test] fn reconcile_starts_only_when_the_switch_is_on_and_no_endpoint_is_configured() {}
```
假 installer 创建二进制文件；假 launcher 在 `spawn` 里绑定一个 localhost `TcpListener` 并由 handle 持有（让 `is_running()` 变真），或故意不绑定以触发超时路径。测试用 `with_ready_timeout(Duration::from_millis(100))` 避免 5 秒等待。

- [ ] **Step 2: 运行并确认失败**

Run: `cargo test -p api --test management obscura`
Expected: 编译失败（无 `with_installer` 等）。

- [ ] **Step 3: 实现 seam 与生产实现**

`obscura_manager.rs` 只保留 trait、manager、`reconcile`、`status`；下载解压与 `Command` 启动移到 `obscura_install.rs`（用 `#[path = "obscura_install.rs"] mod install;` 的既有仓库模式）。两个文件都需遵守 200 行软限制。

- [ ] **Step 4: 运行并确认通过**

Run: `cargo test -p api --test management`
Expected: 全绿。

- [ ] **Step 5: 提交**

```bash
git add crates/api/src/obscura_manager.rs crates/api/src/obscura_install.rs crates/api/tests/management/obscura_lifecycle.rs
git commit -m "feat(api): make the Obscura lifecycle testable and give it real seams"
```

---

### Task 5: Obscura HTTP 路由与保存期 reconcile

**Files:**
- Create: `crates/api/src/http/obscura.rs`
- Modify: `crates/api/src/http/mod.rs`（`admin_ops_routes`，紧邻 `/settings/browser`）, `crates/api/src/http/browser_settings.rs`, `crates/api/src/management/state.rs`（新增 `with_obscura`）
- Test: `crates/api/tests/management/obscura_routes.rs`（新建，并在 `crates/api/tests/management/main.rs` 注册）

**Interfaces:**
- Consumes: Task 4 的 manager/status/reconcile。
- Produces:
  - `GET /api/v1/settings/browser/obscura` → `{installed, running, downloading, port, cdp_url, version, binary_path}`
  - `POST /api/v1/settings/browser/obscura/start` → `{ok:true}` 或 502 `obscura.start_failed`
  - `POST /api/v1/settings/browser/obscura/stop` → `{ok:true}`
  - `GET/PUT /settings/browser` 的响应新增 `"obscura"` 字段（进程事实，来自 manager），`cdp` 段保持 Task 2 形状
  - `ApiState::with_obscura(self, Arc<ObscuraManager>) -> Self`

- [ ] **Step 1: 写失败测试**

```rust
#[tokio::test] async fn obscura_routes_are_admin_only() {}          // member → 三个路由全 403
#[tokio::test] async fn obscura_status_reports_process_facts_not_intent() {}
#[tokio::test] async fn browser_settings_response_merges_obscura_status() {}
#[tokio::test] async fn saving_the_switch_on_without_an_endpoint_starts_the_managed_daemon() {}
#[tokio::test] async fn saving_the_switch_off_stops_the_managed_daemon() {}
#[tokio::test] async fn configuring_an_instance_endpoint_stops_the_managed_daemon() {}
```

- [ ] **Step 2: 运行并确认失败**

Run: `cargo test -p api --test management obscura_routes`
Expected: 404 / 编译失败。

- [ ] **Step 3: 实现路由与保存期 reconcile**

保存期 reconcile 必须在 `state.http_work.run_to_completion(...)` 里跑（安装/启动是阻塞 IO），且在 `store` 锁释放之后；reconcile 失败不改变保存结果（配置已持久化），但要 `error!` 并在响应里带 `obscura_error`。

- [ ] **Step 4: 运行并确认通过**

Run: `cargo test -p api --test management`
Expected: 全绿。

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
  - provider closure 每次读取设置后返回：`enabled = cdp.enabled`；`endpoint = cdp.url` 非空时 `Some`；`managed_endpoint = if enabled && url 为空 { Some(manager.cdp_url()) } else { None }`

**行为约束：**
- `CRAWLER_MEDIA_BROWSER=1` 不再拒绝启动，行为等同打开开关（经 `seed_switch_default` 落成显式设置）。用户已显式设置过 `cdp.enabled` 时，env 不得覆盖。
- 受管端点**不做**每请求探活（`is_running()` 不参与），可用性由连接结果决定——失败即按 Task 2 回退 HTTP 并 warn。这避免每请求一次 TCP 探测。
- `boot_reconcile` 失败只 `error!`，不阻止服务启动。

- [ ] **Step 1: 写失败测试**

```rust
#[test] fn env_flag_seeds_the_switch_only_when_it_is_unset() {}
#[test] fn env_flag_never_overrides_an_explicit_switch_value() {}
#[test] fn production_browser_no_longer_rejects_the_legacy_managed_flag() {}
#[tokio::test] async fn switch_on_without_an_instance_endpoint_passes_the_managed_endpoint() {}
#[tokio::test] async fn switch_on_with_an_instance_endpoint_ignores_the_managed_one() {}
#[tokio::test] async fn switch_off_never_opens_a_browser_session() {}
#[tokio::test] async fn switch_off_ignores_a_site_level_endpoint() {}
#[tokio::test] async fn saving_settings_changes_the_next_request_without_restart() {}
```
删除 `browser_runtime.rs` 中 `production_browser(store.clone(), true).is_err()` 的断言。

- [ ] **Step 2: 运行并确认失败**

Run: `cargo test -p api --test browser_runtime`
Expected: 编译失败（参数个数不符）。

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
- 浏览器签到用导航（GET）访问 `{site_base}attendance.php`，与人工点击链接一致；HTTP 路径保持 POST 不变。
- 两条路径共用同一 `SiteResponsePolicy`（`NexusPhpPolicy`），transport 不影响判定。

- [ ] **Step 1: 写失败测试**

```rust
#[test] fn browser_check_in_verifies_the_business_outcome_from_the_rendered_page() {}
#[test] fn browser_check_in_persists_the_read_back_cookie() {}
#[test] fn browser_login_persists_the_read_back_cookie() {}
#[test] fn browser_page_without_a_read_back_cookie_keeps_the_existing_cookie() {}
#[test] fn browser_check_in_still_rejects_a_challenge_page() {}
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
2. `use_browser = profile.render && state.browser.cdp_enabled().unwrap_or(false)`
3. `use_browser` → 先跑 browser 插件；`Err` → `warn!`（含 `site_id`、`error`、`fallback = "http"`）后跑 HTTP 插件
4. 否则直接跑 HTTP 插件

`site_login` / `site_check_in` / `check_in::run` 三处都改为调用这两个函数，删除各自内联的插件构造。

- [ ] **Step 1: 写失败测试**

```rust
#[tokio::test] async fn render_profile_without_cdp_uses_http_login() {}
#[tokio::test] async fn browser_login_failure_falls_back_to_http_and_succeeds() {}
#[tokio::test] async fn browser_check_in_failure_falls_back_to_http() {}
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
- Produces：`POST /settings/browser/sync-cdp` 的端点来源 = 非空 `cdp.url`，否则受管 Obscura 的 `cdp_url()`；开关关闭 → 400 `cdp.disabled`；受管 Obscura 未运行且无实例端点 → 400 `cdp.no_endpoint`。

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
- Test: `web/test/cdp-browser-settings.test.mjs`（新建）

**Interfaces:**
- Consumes: Task 2 的 `GET/PUT /settings/browser` 形状、Task 5 的三个 Obscura 路由。
- Produces：单一开关 + 可选端点输入 + 受管 Obscura 状态区（installed/running/version/port + 安装并启动 / 停止 按钮）+ 保留 cookie 同步按钮；文案必须说明「关闭时不使用 CDP」「CDP 失败会回退 HTTP」。

**测试模式：** 本仓库 `web/test/*.test.mjs` 用 `node --test` + 源码正则断言（见 `web/test/api-contracts.test.mjs`），不渲染组件。

- [ ] **Step 1: 写失败测试**

```javascript
test("browser settings panel sends one switch and no legacy obscura toggle", async () => {
  const source = await readWeb("components/site-config-section.tsx");
  assert.match(source, /cdp: \{ enabled: cdpEnabled, url: cdpUrl \}/);
  assert.doesNotMatch(source, /obscura: \{ enabled:/);
});
test("browser settings panel surfaces managed obscura process facts", async () => {
  const source = await readWeb("components/site-config-section.tsx");
  assert.match(source, /data\.data\.obscura\?\.running/);
  assert.match(source, /\/api\/v1\/settings\/browser\/obscura\/start/);
});
```

- [ ] **Step 2: 运行并确认失败**

Run: `cd web && node --test test/cdp-browser-settings.test.mjs`
Expected: FAIL。

- [ ] **Step 3: 改造面板**

删除独立的 Obscura 开关与 URL 输入；`cdp.url` 留空即表示使用受管 Obscura，UI 必须显式提示这一点。

- [ ] **Step 4: 运行并确认通过**

Run: `cd web && node --test && pnpm typecheck`
Expected: 全部通过。

- [ ] **Step 5: 提交**

```bash
git add web/components/site-config-section.tsx web/test/cdp-browser-settings.test.mjs
git commit -m "feat(web): one CDP switch with managed Obscura status and controls"
```

---

## 收尾

- [ ] `cargo test --workspace` 全绿。
- [ ] `cd web && node --test && pnpm typecheck` 全绿。
- [ ] 按 Review Focus 六条逐条确认对应测试存在且真的在断言该行为。
- [ ] 确认 `docs/adr/0007-cdp-browser-opt-in.md`、`CONTEXT.md`、`docs/deploy.md` 的描述与最终实现一致；如有偏差，改代码或改文档，不允许两边不一致。
- [ ] 在 worktree 提交完整分支，`main` 保持干净。
