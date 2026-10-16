# CDP/Obscura 受管浏览器链路 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让实例级单一 CDP 开关成立：开启时按 Site `cdp_url` → 实例 `cdp.url` → 受管 Obscura 解析端点，**仅用于 Site 的登录与 Check-in**（导航前失败回退 HTTP）；关闭时完全不使用 CDP；浏览器会话持久化真实回读的 cookie。**搜索与 RSS 永远走 HTTP，不使用 CDP。**

**Architecture:** 端点解析下沉到 `crates/indexer` 的 `BrowserConfig::resolve` 与 `Browser`（只被 hooks 的登录/签到插件调用），Indexer 搜索路径只用 HTTP `Fetcher`，删除 `RoutedFetcher` 与 `FetchRequest.render`；`crates/api` 通过 provider closure 在每次请求读取实时设置，并把受管 Obscura 的本地端点作为第三级兜底注入。登录/签到的 transport 选择（按 Site 的 `maintenance_via`）与失败回退由 `crates/api` 的新模块统一编排，业务判定复用 `crates/hooks` 既有的 `SiteResponsePolicy`，因此 transport 不影响「签到成功」的结论。

**Tech Stack:** Rust 2024 workspace（`indexer` / `hooks` / `api`）、axum、rusqlite、tungstenite、thiserror、tracing；前端 Vite + React + `node --test`。

**Spec:** `docs/adr/0007-cdp-browser-opt-in.md`（决策）、`CONTEXT.md` 的 **Browser** 词条、`docs/deploy.md` 的部署边界。执行前先读这三处。

## 修订记录

- 2026-10-16 r2：按审核报告（B1–B2、H1–H5、M1–M7、L1–L8）修订。主要变化：新增 Task 0 兼容性 spike；Task 1 改为自足的完整实现步骤（不再依赖本机补丁）；cookie 注入改为 `Network.setCookies` 并按 host 绑定；受管 Obscura 改为哈希校验安装、随机端口 + 身份确认、按需启动、Job 化 reconcile；回退语义在数据层可见。

- 2026-10-16 r3：按复审（R2-H1、R2-M1–M4、R2-L1–L4）修订。新增 Task −1（本提交已完成）同步 ADR/deploy/CONTEXT；并发上限与 T0 隔离能力联动；https 存量端点迁移提示；`CRAWLER_MEDIA_BROWSER` 未启动原因；v1 内实现 render 总时限。

- 2026-10-16 r4 补记：r2 修订时 Global Constraints 与 Review Focus 两节被误删，r4 已恢复并按新范围更新。
- 2026-10-16 r4：**范围调整**（用户决定）：删除搜索渲染——搜索/RSS 永不使用 CDP，Browser 只服务登录与 Check-in。删除 Profile `render` 搜索路径、`RoutedFetcher`、`FetchSource`/`HttpFallback`、`SiteFailureKind::CdpFallbackEmpty` 及其 UI、插件 Profile `render` 拒绝；新增 Site 级 `maintenance_via`（`http`/`browser`），受管 daemon 在 `browser_sites > 0` 时按需启动。多标签并发：cookie 清理改为按会话站点 `Network.deleteCookies`（不依赖 `createBrowserContext`），删除 `CookieIsolation`/强制并发 1；默认并发 3，permit 有界等待；同站点会话串行（per-site 锁）；清理与 close 有独立上限。R3-M1（多 render 站点搜索必然降级）随搜索渲染删除而**不再适用**。

- 2026-10-16 r5：按第四轮复审修订。R4-H1：核对代码（`check_in.rs::run`、`http/sites.rs` 的 `site_login`/`site_check_in` 只用 `LoginPlugin::http`/`CheckInPlugin::http`，`LoginVia::Browser` 仅在 hooks 测试中使用），旧版本登录/签到从未走浏览器——迁移一律 `maintenance_via = http`，删除「有 Site `cdp_url` 就打开开关」，改为提示用户手动选择。R4-M1：清理改用 `Network.getAllCookies`（或 `Storage.getCookies`）按 domain 过滤后以 cookie 自身 `domain`+`path` 精确删除；同站点锁按可注册域（后缀互含）分组，忽略 scheme/端口。R4-M2：独立排队上限 + 剩余时间下限 + FIFO 票号。R4-M3：旧 render overlay 站点可见提示。R4-L1–L4。

- 2026-10-16 r6：按第五轮复审修订。R5-M1：分组改用 `psl` crate 的可注册域（eTLD+1），IP/单段名/`localhost` 用整个 host（`*.localhost` 归入 `localhost`），补齐堂兄弟、跨层级、IDN 等用例。R5-M2：统一 `normalize_host`（小写、去尾点、`idna` 转 ASCII、IPv6 去方括号），分组、cookie 筛选、`visited_hosts` 共用；T0 加 IDN cookie domain 实测。R5-L1 只记主 frame 导航、跨组 host 告警；R5-L2 时间参数校验；R5-L3 proxy/browser 双向不变式；R5-L4 `no_managed_browser_sites`。

## 起点

分支 `feat/cdp-obscura` 自 `d201fcf` 起，代码与 `main` 相同。**不存在、也不依赖任何本机补丁**；Task 1 从现有代码（`crates/indexer/src/browser.rs` 中 `PageSession::cookie_header` 默认返回 `Ok("session=browser")`、`cdp_page.rs` 未实现 `cookie_header`、`set_cookie_header` 走 `Network.setExtraHTTPHeaders`）直接改起。

依赖链：T−1（已完成）→ T0 → T1 → T2 → T3；T4 → T5 → T6；T1 → T7 → T8（依赖 T6 的 `ApiState.browser`）；T9 依赖 T2、T4；T10 依赖 T2、T5。T0 的结论若否定某个 CDP 方法，先改 T1 的降级方式再开工。

## Global Constraints

- 工作目录：`.worktrees/cdp-obscura`，分支 `feat/cdp-obscura`。所有改动在此 worktree 内，`main` 保持干净。
- 每个被触及的 crate 先 `cargo test -p <crate>`；全部完成后跑一次 `cargo test --workspace`。
- 测试不得访问实网、不得启动真实 Chromium/Obscura（Task 0 的 `#[ignore]` 探针除外）。CDP 用 localhost 假服务，安装/启动/HTTP 用注入 trait。
- `indexer` 不得依赖 `api`/`store`；`hooks` 不得依赖 `api`；`domain` 仅类型无 IO。
- 尺寸硬限制：`.rs` 文件 800 行（软 200）、生产函数 120 行（软 60）、测试函数 120 行（软 80）、`lib.rs` 软 150。超软限需在 PR 描述里说明理由。
- 无 `unwrap`/`expect` 于 IO 或解析路径（测试除外）；错误用 `thiserror`。顺手修复触及文件中的既有违例（如 `obscura_manager.rs:51` 的 `.parse().unwrap()`）。
- 结构化日志：`error!` 不可预期失败；`warn!` 可恢复降级——**本次所有 CDP 回退必须 warn**；`info!` 生命周期（安装/启动/停止/登录/签到）；`debug!` 细节。**cookie 明文绝不进日志**。
- 期望值按具体用例断言（用例名 + 断言内容），不写「N passed」这类依赖本机状态的计数。
- 改动公共签名（尤其 Task 2 删除 `RoutedFetcher`、Task 6 的 `production_browser`）前先跑 `cargo check --workspace --all-targets`，覆盖 `crates/api/src/bin/live_loop`、`fs_watcher_tests` 等间接调用方。
- **范围**：Browser 只服务 Site 的登录与 **Check-in**；搜索与 RSS 永远走 HTTP（r4）。
- 术语遵循 `CONTEXT.md`：**Browser**、**Check-in**、**Site**、**Indexer**、**Profile**、**Torrent**、**Plugin**。
- 行为变更与 ADR-0007 / `docs/deploy.md` / `CONTEXT.md` 在同一提交内同步。
- 每个 task 结束时提交一次，`feat:` / `refactor:` / `test:` 前缀。不提交无关文件。

## Review Focus

以下输入/失败模式最容易伤人，且没有现成测试覆盖。每条都已在对应 task 里安排了测试，实现时不要跳过。

1. **开关关闭但 Site 配了 `cdp_url` 或 `maintenance_via = browser`**：必须证明 HTTP 被使用且 opener 从未被调用。
2. **搜索绝不打开 Browser**：即使 overlay Profile 写了 `render: true`（Task 2）。
3. **`Network.getCookies` 返回同名多条**（不同 path/domain）：拼出的 header 必须按 name 去重（Task 1）。
4. **受管 Obscura 未就绪**（未安装/下载失败/端口被外部服务占用）：不得杀外部进程，不得把外部进程当受管端点，请求必须回退 HTTP 而不是挂死（Task 4/6）。
5. **浏览器路径与 HTTP 路径的登录/签到判定必须同源**：同一个 `SiteResponsePolicy`（Task 7）。
6. **端点非法**（`https`、`ws://`、内嵌凭据、带 query）：写入时 400 拒绝；存量非法端点只提示不改写（Task 2/3）。
7. **受管二进制被篡改**：哈希不符、超过大小上限、归档含意外条目，一律拒绝安装且不留文件（Task 4）。
8. **站点 cookie 外泄与串号**：cookie 只经 `Network.setCookies` 绑定站点 host，不出现在 `setExtraHTTPHeaders`；会话结束按 cookie 自身 domain+path 删除本站点（含父域、跳转 host）cookie，不清整个 jar；同一可注册域（`psl` 判定，host 先规范化，含大小写/尾点/IDN）的会话串行；只清理主 frame 访问过的 host（Task 1）。
9. **导航后失败的重复签到**：导航已发出后失败不得再走 HTTP 签到（Task 8）。
10. **升级不得改变登录/签到方式**：迁移后所有 Site 仍为 `http`，开关不被自动打开，旧 `cdp_url`/render 配置只产生提示（Task 2）。

---

### Task −1: 同步规格文档（ADR-0007 / deploy / CONTEXT）——**已在 r3 提交中完成**

执行者开工前要读的三份规格必须与本计划一致。r3 提交已改写：

- `docs/adr/0007-cdp-browser-opt-in.md`：默认 `preinstalled`、显式 `download`；随机端口 + 回环绑定 + 身份确认；端点只接受 `http`；登录/签到导航后失败不回退；受管 daemon 仅在 `managed_browser_sites > 0` 时启动（r4/r5）；搜索不使用 CDP（r4）；新增「并发与 cookie 隔离」「信任模型」「迁移规则」（含 https 存量端点）；「验证边界」留出 Task 0 实测表。
- `docs/deploy.md`：Browser 段落与环境变量表（`CRAWLER_MEDIA_BROWSER` 只控制开关、新增 `CRAWLER_MEDIA_OBSCURA_INSTALL`）、预置规则、`network_mode: host` 风险。
- `CONTEXT.md` 的 **Browser** 词条。

- [x] **Step 1: 改写三份文档**（r3 提交）
- [ ] **Step 2: 后续 task 若改变行为，同一提交内同步改这三份文档**；Task 0 只向 ADR「验证边界」追加实测结果。

---

### Task 0: Obscura CDP 兼容性 spike（手动 / `#[ignore]`，不进 CI）

现有客户端依赖的 CDP 能力在 Obscura（非 Chromium）上未经验证。本 task 产出事实，不产出生产代码。

**Files:**
- Create: `crates/indexer/tests/obscura_compat.rs`（全部 `#[ignore]`，读 `OBSCURA_COMPAT_URL` 环境变量；未设置即 skip）
- Create: `crates/indexer/tests/fixtures/browser/index.html`、`attendance.html`（模拟 NexusPHP 首页与签到页，由带 `Set-Cookie` 的本地服务提供）
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
`PUT /json/new`（及 GET 回退）、`PUT /json/close/{id}`、`webSocketDebuggerUrl` 的 scheme/host/port、`Network.enable`、`Page.enable`、`Page.setLifecycleEventsEnabled` + `Page.lifecycleEvent(load)`、`Page.navigate`（`frameId`/`loaderId`）、`Runtime.evaluate`（`returnByValue`）、`Network.setCookies`、`Network.getCookies{urls}`、`Network.deleteCookies{name,url}`（**必需**：删除后 `getCookies` 应为空；同时另一 tab 中另一站点的 cookie 不受影响）、`Network.getAllCookies` 与 `Storage.getCookies`（至少一个可用）、带 `domain`+`path` 的 `Network.deleteCookies` 能删除 `path=/forum` 的 cookie、服务器设置父域 cookie（`Domain=.example.test`）后兄弟子域 tab 是否可见、`Target.createBrowserContext` / `Target.disposeBrowserContext`（可选，仅记录）、两个 tab 并发导航不同站点（R4-M1）、IDN 站点（本地 hosts 映射 `例子.test` → 127.0.0.1，服务端 `Set-Cookie: Domain=例子.test` 与 `Domain=xn--fsqu00a.test` 各一次）`getAllCookies` 返回的 cookie domain 形式——预期为小写 punycode，记录实际形式（R5-M2）。另测一次 NexusPHP 签到页：浏览器 GET `attendance.php` 是否真实签到（L7）。

- [ ] **Step 4: 写入 ADR「验证边界」**

表格：方法 → 支持/不支持 → 降级方式。约定的降级：
- 无 `Target.createBrowserContext` → 无影响：v1 用按站点 `Network.deleteCookies` 清理，不依赖独立上下文；独立上下文是后续增强。
- 无 `Network.getAllCookies` 且无 `Storage.getCookies`，或带 domain+path 的 `deleteCookies` 删不掉 path 级 cookie → 无法精确清理：`Browser` 并发上限**降为 1**（`with_max_sessions` 被钳制），会话结束仍尽力按 `getCookies{urls}` 清理，并在 ADR 写明这一前提。
- 无 `Network.deleteCookies` → 改为对每个待删 cookie 发 `Network.setCookie{name, domain, path, expires: 1}` 覆盖删除；仍不可行同样降为并发 1。
- 无 `Network.getCookies` → `cookie_header` 返回 `Err`，`RenderedPage.cookie_header = None`（保留原 cookie）。
- 无 `Network.setCookies` → **不渲染带 cookie 的请求**（报 `CdpUnsupported`，回退 HTTP），绝不退回 `setExtraHTTPHeaders`。
- 无 lifecycle 事件 → 仅以 `document.readyState == complete` 判定。
- 浏览器 GET 不真实签到 → Task 7 浏览器签到改为在页面内提交签到表单/按钮（`Runtime.evaluate` 触发 `form.submit()`），或该站点禁用浏览器签到。

若关键方法（`/json/new`、`Page.navigate`、`Runtime.evaluate`、`Network.setCookies`）任一不支持，**停止本计划**，回到 ADR 重新决策。

- [ ] **Step 5: 提交**

```bash
git add crates/indexer/tests/obscura_compat.rs crates/indexer/tests/fixtures/browser docs/adr/0007-cdp-browser-opt-in.md
git commit -m "test(indexer): add ignored Obscura CDP compatibility probe and record results"
```

---

### Task 1: indexer — `Browser::render`、cookie 按 host 注入/回读/按站点清理、多标签并发

`Browser` 只被 hooks 的登录/签到调用（Task 7），搜索不经过它。

**Files:**
- Modify: `crates/indexer/src/browser.rs`
- Create: `crates/indexer/src/browser_slots.rs`（全局并发上限 + 同站点锁，用 `#[path]` 挂在 `browser.rs` 下）
- Modify: `crates/indexer/src/cdp_page.rs`
- Modify: `crates/indexer/src/cdp_page_transport.rs`（`Transport::connect` 的 scheme 校验；`TIMEOUT` 单步上限保留，新增接受外部 deadline 的入口）
- Create: `crates/indexer/src/browser_host.rs`（`normalize_host`、`group_key`，用 `#[path]` 挂在 `browser.rs` 下）
- Modify: `crates/indexer/Cargo.toml`（新增 `psl = "2"`、`idna = "1"`；`idna 1.1.0` 已在 `Cargo.lock` 中作为 `url` 的依赖）
- Modify: `crates/indexer/src/lib.rs:9`
- Test: `crates/indexer/tests/cdp_page.rs`, `crates/indexer/tests/browser.rs`, `crates/indexer/tests/browser_host.rs`

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
        /// 删除 domain 属于 `hosts`（本站 host、其父域、会话中访问过的 host）的全部 cookie；默认 no-op。
        fn clear_site_cookies(&self, hosts: &[String], deadline: Instant) -> Result<CleanupReport, IndexerError> {
            let _ = (hosts, deadline);
            Ok(CleanupReport::default())
        }
        /// 会话中实际访问过的 host（含跳转），用于清理；默认空。
        fn visited_hosts(&self) -> Vec<String> { Vec::new() }
        fn set_deadline(&self, deadline: Instant) { let _ = deadline; }
    }
    ```
    删除 `set_cookie_header`（及其 `Network.setExtraHTTPHeaders` 实现）。
  - `#[derive(Clone)] pub struct Browser`（`config`、`provider`、`opener` 已是 `Arc`/`Clone`，另加 `slots: Arc<BrowserSlots>`）
  - `Browser::render(&self, request: &FetchRequest) -> Result<RenderedPage, IndexerError>`；开关关闭时返回 `BrowserSetup("CDP disabled")`（Task 8 先查开关，正常不会走到）
  - 删除 `Browser::fetch_html`（唯一调用方 `hooks/src/plugins.rs::browser_page` 在 Task 7 改用 `render`）
  - `Browser::with_max_sessions(self, n: usize) -> Self`：全局同时打开的 tab 数，**默认 3**，允许 1–8
  - `Browser::with_render_deadline(self, Duration) -> Self`：默认 30s（登录/签到是后台任务），**包含排队等待**
  - `Browser::with_queue_cap(self, Duration) -> Self`：排队上限，默认 15s（R4-M2）
  - `Browser::with_min_work(self, Duration) -> Self`：拿到名额后至少要剩的工作时间，默认 10s（R4-M2）
  - 时间参数约束（R5-L2）：`Browser::validate_timing(&self) -> Result<(), IndexerError>`，要求 `min_work + 1s ≤ render_deadline` 且 `queue_cap ≤ render_deadline − min_work`；三个 `with_*` 只记录值，`Browser` 首次 `render` 前与 api 构造 `production_browser` 时调用校验，违反即返回配置错误（不静默钳制）
  - `Browser::with_precise_cleanup(self, bool) -> Self`：默认 `true`；Task 0 证明不能精确清理时由 api 设为 `false`，此时有效并发上限钳制为 1（R4-M1）
  - `Browser::with_cleanup_cap(self, Duration) -> Self`：默认 2s，是清理 cookie 和关闭 target 的**独立**上限，不占 render 预算（R3-L4）
  - `IndexerError::BrowserBusy` 与 `IndexerError::BrowserSetup(String)` 变体：排队超时、`open`、`set_site_cookies` 等**导航前**失败都归为 setup 阶段（Task 7/8 据此决定是否允许回退）

**多标签与站点隔离（r4/r5，不依赖 `Target.createBrowserContext`）：**
- 所有 tab 共用 daemon 的默认 cookie jar。我们注入的 cookie 用 `url` 字段做 host-only 绑定；但服务器在会话中可能写入父域 cookie（`Domain=.example.org`），host-only 只隔离我们注入的部分，因此还需要下面的锁粒度与精确清理。
- **host 规范化（R5-M2）**：`normalize_host(raw: &str) -> Result<String, IndexerError>`：去掉 IPv6 方括号 → 去掉尾点 → `idna::domain_to_ascii`（同时完成小写与 Unicode→punycode；失败即 `BrowserSetup`）；IP 字面量（`std::net::IpAddr` 可解析）只做小写/去括号，不走 IDNA。站点 host、cookie 的 `domain`（先去前导 `.`）、`visited_hosts` 一律先经过它再比较。
- **按可注册域分组串行（R5-M1）**：`group_key(host) -> String`：先 `normalize_host`；IP 字面量、单段名（无 `.`）→ 整个 host；`localhost` 与 `*.localhost` → `"localhost"`；其余用 `psl::domain_str(host)` 取可注册域（eTLD+1），取不到（host 本身就是公共后缀等）→ 整个 host。`BrowserSlots` 持有 `Mutex<SlotState{active, groups: Vec<String>, queue: VecDeque<u64>, next_ticket}>` 和一个 `Condvar`，**同一 group_key 同时最多一个会话**；忽略 scheme 与端口（cookie 不按它们隔离）。`psl` 把公共后缀表编译进二进制，运行时不联网；`*.co.uk`、`*.com.cn` 下不同可注册域的站点可以并发。
- **全局上限**：`active < effective_max`，`effective_max = if precise_cleanup { max_sessions } else { 1 }`。
- **有界等待 + FIFO（R4-M2）**：`acquire` 先领票号入队，只有队首且满足名额与分组条件时才出队（被分组阻塞的队首不阻塞后面不冲突的票，按票号顺序扫描第一个可运行的）。最长等待 `min(queue_cap, render_deadline − min_work)`；超时返回 `BrowserBusy`。拿到名额时若剩余时间 `< min_work`，立即释放并返回 `BrowserBusy`。两种情况都发生在 opener 调用之前，属于**导航前失败**，可回退 HTTP。
- **精确清理（R4-M1）**：会话结束时 `Network.getAllCookies`（不可用时 `Storage.getCookies`）取全部 cookie，筛出 `domain`（去掉前导 `.`）等于或是 `hosts` 中任一 host 的父域/子域的条目，对每条按其**自身的** `name`+`domain`+`path` 发 `Network.deleteCookies{name, domain, path}`。删除后再查一次，残留则 `warn!(remaining)`。**不使用 `Network.clearBrowserCookies`**。由于同组串行，删除父域 cookie 不会影响正在运行的兄弟子域会话。
- `hosts` = 本站 host ∪ `visited_hosts()`。`visited_hosts` **只记录主 frame**（`Page.frameNavigated` 中 `frame.parentId` 为空）导航到的 host，覆盖 SSO 跳转，不收 iframe（广告、`challenges.cloudflare.com` 等共享第三方），避免误删别的会话正在用的第三方 cookie（R5-L1）。访问到的 host 的 `group_key` 与本会话不同时 `warn!(host, "Browser session left its cookie group")`；ADR 写明跨分组共享的 SSO/质询主机不保证隔离，需要时把并发设为 1。
- 清理和 `/json/close` 都在**释放分组与全局名额之前**完成，各自受 `cleanup_cap` 约束，超时只 `warn!`。单个会话最长占用 = `render_deadline + 2 × cleanup_cap`。
- 每会话独立的 `Target.createBrowserContext` 留作后续增强（Task 0 只记录能力），v1 不实现。

- [ ] **Step 1: 写失败测试（cookie 注入与清理）**

`crates/indexer/tests/cdp_page.rs` 的假 CDP 服务已记录收到的 method；新增：

```rust
#[test]
fn site_cookies_are_bound_to_the_site_host_not_sent_as_extra_headers() {
    // render(url = "http://127.0.0.1:{port}/index.php", cookie = "uid=1; pass=abc")
    // 断言：收到 Network.setCookies，params.cookies ==
    //   [{name:"uid",value:"1",url:"http://127.0.0.1:{port}/"},{name:"pass",value:"abc",url:...}]
    // 断言：从未收到 Network.setExtraHTTPHeaders
}
#[test]
fn https_site_cookies_are_marked_secure() {}
#[test]
fn session_end_deletes_this_site_cookies_by_their_own_domain_and_path() {
    // getAllCookies 返回：sid(domain a.example.test, path /)、fid(domain a.example.test, path /forum)、
    //   pd(domain .example.test, path /)、other(domain other.test)
    // 断言：收到 deleteCookies{sid,a.example.test,/}、{fid,a.example.test,/forum}、{pd,example.test,/}，没有 other
    // 断言：从未收到 Network.clearBrowserCookies
}
#[test]
fn cookies_from_visited_redirect_hosts_are_cleaned_too() {
    // Page.frameNavigated 到 sso.example2.test；该 host 的 cookie 也被删除
}
#[test]
fn falls_back_to_storage_get_cookies_when_get_all_cookies_is_missing() {}
#[test]
fn residual_cookies_after_cleanup_only_warn() {}
#[test]
fn cleanup_is_capped_and_failures_only_warn() {
    // deleteCookies 挂起；with_cleanup_cap(100ms)；render 结果不受影响，总耗时 ≤ deadline + 2×cap
}
```

- [ ] **Step 2: 写失败测试（回读、去重与并发）**

假服务的 `Network.getCookies` 新增一个模式：校验请求 `params.urls == [当前页 URL]`，返回同名不同 path 的两条（`sid=old` path `/`、`sid=new` path `/forum`）以及 `uid=1`。

```rust
#[test]
fn cookie_readback_requests_cookies_for_the_current_page_url() {}
#[test]
fn cookie_readback_deduplicates_same_name_across_paths() {
    // 同名时后出现者覆盖（与 crates/indexer/src/cdp.rs:219 的 name_map.insert 一致），再按 name 排序
    assert_eq!(page.cookie_header.as_deref(), Some("sid=new; uid=1"));
}
#[test]
fn readback_failure_yields_none_and_keeps_the_html() {}
#[test]
fn default_page_session_cookie_header_is_an_error_not_a_fake_session() {
    // 只实现必需方法的假 PageSession；断言 cookie_header(..).is_err()；
    // 并 grep 断言 browser.rs 不再含 "session=browser"
}
```

`crates/indexer/tests/browser.rs` 新增：

```rust
#[test] fn render_returns_html_and_read_back_cookie() {}
#[test] fn target_is_closed_when_goto_fails() {}
#[test] fn websocket_url_on_a_different_host_or_port_is_rejected() {
    // /json/new 返回 webSocketDebuggerUrl = ws://10.0.0.9:9/devtools/page/x → Err，且 target 仍被 close
}
#[test] fn different_sites_render_concurrently_up_to_the_limit() {
    // with_max_sessions(3)；三个不同站点的会话同时阻塞在 barrier 上 → 三个 tab 同时打开
}
#[test] fn extra_session_waits_then_runs_when_a_slot_frees() {
    // with_max_sessions(1)；第二个站点排队，第一个结束后完成；两者的 cookie 互不干扰
}
#[test] fn waiting_beyond_the_queue_cap_is_busy_before_navigation() {
    // with_max_sessions(1) + with_queue_cap(100ms)；第一个占用 300ms → 第二个 Err(BrowserBusy)，其 opener 从未被调用
}
#[test] fn too_little_time_left_after_queueing_is_busy_before_navigation() {
    // render_deadline 300ms、min_work 200ms、queue_cap 1s；第一个占用 150ms → 第二个拿到名额时剩余 < 200ms → BrowserBusy，opener 未被调用
}
#[test] fn waiters_are_served_in_fifo_order() {
    // max 1；依次提交 3 个不同站点；完成顺序与提交顺序一致
}
#[test] fn same_site_sessions_are_serialized() {
    // 同一 host 不同端口/scheme 的两个 render：opener 记录的最大并发为 1
}
#[test] fn unrelated_two_label_hosts_run_concurrently() {}
#[test] fn cousin_and_cross_level_subdomains_are_serialized() {
    // x.a.example.test / y.b.example.test；tracker.example.test / x.forum.example.test → 最大并发 1
}
#[test] fn timing_constraints_are_validated() {
    // render_deadline 10s + min_work 10s → 配置错误；queue_cap 25s + deadline 30s + min_work 10s → 配置错误；默认值通过
}
#[test] fn sibling_subdomains_are_serialized() {
    // a.example.test 与 b.example.test：最大并发 1；第一个删除 .example.test 父域 cookie 时第二个尚未开始
}
#[test] fn iframe_navigations_are_not_cleaned() {
    // 子 frame（parentId 非空）导航到 challenges.cloudflare.test：其 cookie 不被删除
}
#[test] fn main_frame_redirect_to_another_group_warns() {}
#[test] fn imprecise_cleanup_clamps_concurrency_to_one() {
    // with_precise_cleanup(false) + with_max_sessions(3)：不同站点最大并发 1
}
#[test] fn render_respects_the_total_deadline() {
    // with_render_deadline(200ms)；假 CDP 每步延迟 80ms
    // 断言返回超时错误，总耗时 < 200ms + 2×cleanup_cap，target 仍被 close
}
```

`crates/indexer/tests/browser_host.rs` 新增（R5-M1/M2，第五轮复审第二节表格中的全部例子）：

```rust
#[test] fn normalize_host_lowercases_strips_trailing_dot_and_brackets() {
    // "Example.ORG." → "example.org"；"[::1]" → "::1"
}
#[test] fn normalize_host_converts_idn_to_punycode() {
    // "例子.中国" → "xn--fsqu00a.xn--fiqs8s"
}
#[test] fn group_key_cases() {
    // 同组：a.example.org/b.example.org；a.example.org/example.org；a.b.co.uk/c.b.co.uk；a.b.co.uk/b.co.uk；
    //       x.a.example.org/y.b.example.org（堂兄弟）；tracker.example.org/x.forum.example.org（跨层级）；
    //       a.example.com.cn/x.b.example.com.cn；Example.ORG/example.org；example.org./example.org；
    //       例子.中国/xn--fsqu00a.xn--fiqs8s；localhost/foo.localhost；pt.x.org:8443/pt.x.org（调用方已去端口）
    // 不同组：example.org/other.org；x.co.uk/y.co.uk；xx.com.cn/yy.com.cn；a.xx.com.cn/b.yy.com.cn；
    //       10.0.0.1/10.0.0.2；1.2.3.4/5.2.3.4；jackett/prowlarr
}
#[test] fn cookie_filter_matches_normalized_domains() {
    // cookie domain ".Example.ORG"、"xn--fsqu00a.xn--fiqs8s" 分别能被 host "example.org"、"例子.中国" 选中并删除
}
```

- [ ] **Step 3: 运行并确认失败**

Run: `cargo test -p indexer --test cdp_page --test browser --test browser_host`
Expected: 编译失败（没有 `render`/`RenderedPage`/`set_site_cookies`/`BrowserBusy`）。

- [ ] **Step 4: 实现 `cdp_page.rs`**

1. `set_site_cookies(page_url, header)`：
   - 用 `tungstenite::http::Uri` 解析 `page_url`（URL 语法解析仍用已有的 `Uri`；host 规范化另用 `idna`，见上文 `normalize_host`），取 `origin = scheme://<normalize_host(host)>[:port]/`；scheme 不是 http/https 或没有 host 时报错。
   - `header.split(';')` → `trim` → `split_once('=')`，跳过空 name；值原样保留，不解码。
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
   （`BTreeMap` 同时完成去重和排序。）
3. `clear_site_cookies(hosts, deadline)`：`Network.getAllCookies`（方法不存在时 `Storage.getCookies`）取全部 cookie；保留 `normalize_host(domain.trim_start_matches('.'))` 与 `hosts`（已规范化）中任一 host 相等或互为 `.` 边界后缀的条目（规范化失败的 cookie 跳过并计入残留）；对每条发 `Network.deleteCookies{name, domain: <cookie 自身 domain>, path: <cookie 自身 path>}`（CDP 按 domain+path 精确匹配；仅给 `url` 时只能删掉适用于该 URL 的 cookie，删不到 `path=/forum` 这类，故不用 `url`）。删除后再取一次，统计残留，返回 `CleanupReport{deleted, remaining}`。日志只记计数。另外在 transport 事件循环中处理 `Page.frameNavigated`：仅当 `frame.parentId` 为空（主 frame）时，把 `normalize_host(frame.url 的 host)` 记入 `visited_hosts`。
4. `open_cdp_session`：拿到 `webSocketDebuggerUrl` 后，校验它的 host 和 port 与 discovery `base` 一致（`ws` 与 `http` 的默认端口都是 80），不一致即 `Err`（H4）。此时 `session` 已构造，`Drop` 会 close target。
5. `Drop`：只做 `transport.close()` 和 `/json/close`（`http_agent` 的超时取 `cleanup_cap`，在构造 session 时记录），失败只 `warn!`。cookie 清理由 `Browser::render` 显式调用，不放在 Drop 里（Drop 没有 deadline 语义）。

- [ ] **Step 5: 处理 HTTPS discovery 与 `wss`（H1）**

当前 `Transport::connect`（`cdp_page_transport.rs:39-44`）只接受 `ws`，而 `validate_endpoint` 接受 `https`。本计划选择**收窄**：`validate_endpoint` 只接受 `http`，错误文案改为「仅支持 http CDP discovery（建议仅回环或受信内网）」；`response().capabilities.endpoint_schemes` 为 `["http"]`。`wss` 支持（tungstenite 的 `rustls-tls-webpki-roots` feature）列为后续工作。新增测试 `endpoint_validation_rejects_https_until_wss_is_supported`。

- [ ] **Step 6: 实现 `browser.rs` 与 `browser_slots.rs`**

```rust
pub fn render(&self, request: &FetchRequest) -> Result<RenderedPage, IndexerError> {
    let deadline = Instant::now() + self.render_deadline;
    let host = normalize_host(&uri_host(&request.url)?)?;               // 出错即 BrowserSetup；忽略 scheme/端口
    let slot = self.slots.acquire(&group_key(&host), deadline, self.queue_cap, self.min_work)?; // FIFO 有界等待；超时或剩余 < min_work → BrowserBusy
    let page = self.open_until(request.cdp_url.as_deref(), deadline)?;  // 失败时尚无 target
    page.set_deadline(deadline);
    let result = (|| {
        if let Some(cookie) = &request.cookie {
            page.set_site_cookies(&request.url, cookie).map_err(setup)?;
        }
        page.goto(&request.url)?;                                      // 此后的失败属于「导航后」
        let html = page.content()?;
        let cookie_header = page.cookie_header(&request.url)
            .inspect_err(|error| tracing::debug!(%error, key = %request.key, "Browser cookie readback unavailable"))
            .ok();
        Ok(RenderedPage { html, cookie_header })
    })();
    let mut hosts = vec![host.clone()];
    hosts.extend(page.visited_hosts());
    match page.clear_site_cookies(&hosts, Instant::now() + self.cleanup_cap) {
        Ok(report) if report.remaining > 0 => tracing::warn!(remaining = report.remaining, key = %request.key, "Browser site cookies left after cleanup"),
        Ok(_) => {}
        Err(error) => tracing::warn!(%error, key = %request.key, "Browser site cookie cleanup failed"),
    }
    drop(page);   // /json/close，受 cleanup_cap 约束
    drop(slot);   // 清理与 close 完成后才释放站点锁和全局名额
    result.inspect_err(|error| tracing::error!(%error, request_key = %request.key, "Browser session failed"))
}
```
总时限：deadline 经 `open_cdp_session_until(endpoint, deadline)` 和 `set_deadline` 传入；`with_transport` 取 `min(Instant::now() + TIMEOUT, deadline)`，HTTP discovery 的 `http_agent` 超时取剩余时间。排队花掉的时间计入同一个 deadline，但排队本身另受 `queue_cap` 约束，且拿到名额后必须剩余 `min_work`，否则在导航前返回 busy（R4-M2）。

`BrowserSlots`（约 90 行）：`acquire(group_key, deadline, queue_cap, min_work)` 领取票号入 `VecDeque`；循环中按票号顺序找出第一个「名额未满且其 group_key 不在运行中分组内」的票，若是自己则出队运行，否则 `cvar.wait_timeout(guard, remaining)`；等待截止 = `min(now + queue_cap, deadline − min_work)`，超时出队并返回 `BrowserBusy`；出队后再检查 `deadline − now >= min_work`。分组只比较 `group_key` 是否相等（不再有 `hosts_conflict` 后缀判定），`browser_host.rs` 单独单测。RAII guard 的 `Drop` 中减计数、移除分组并 `notify_all`。不引入 tokio。删除 `PageSession::set_cookie_header` 的所有实现与调用；`Browser` 加 `#[derive(Clone)]`。

- [ ] **Step 7: 导出**

`crates/indexer/src/lib.rs:9`（`RoutedFetcher` 在 Task 2 删除）：

```rust
pub use browser::{Browser, BrowserConfig, PageSession, RecordingFetcher, RenderedPage, RoutedFetcher};
```
`IndexerError` 增加 `#[error("Browser busy")] BrowserBusy` 与 `#[error("Browser setup: {0}")] BrowserSetup(String)`。

- [ ] **Step 8: 运行并确认通过**

Run: `cargo test -p indexer`
Expected: Step 1、2、5 的新用例全部通过；既有 `cdp_page`、`browser`、`goldens` 用例不回归。

- [ ] **Step 9: 提交**

```bash
git add crates/indexer/src crates/indexer/tests
git commit -m "feat(indexer): bind site cookies per host, clean them per site and run bounded multi-tab Browser sessions"
```

---

### Task 2: 单开关、搜索不再使用 CDP、Site `maintenance_via` + api 设置模型与迁移

本 task 同时改 `indexer`、`domain`、`store`、`hooks` 和 `api`：`BrowserConfig` 换形、`RoutedFetcher` 删除后，`crates/api` 立刻编译失败，分开提交会让中间提交处于红状态。

**Files:**
- Modify: `crates/indexer/src/browser.rs`（删除 `RoutedFetcher`）, `crates/indexer/src/lib.rs`, `crates/indexer/src/fetch.rs`（删除 `FetchRequest.render`，以及 `fetch.rs:123,137,169,183` 处的赋值）, `crates/indexer/src/profile.rs:138`（`render` 字段）
- Modify: `crates/domain/src/lib.rs:134`（`Site` 增加 `maintenance_via`，同步手写的 `Debug`）, `crates/store/src/schema.rs`（新列与迁移）, store 中读写 `sites` 的位置
- Modify: `crates/api/src/main.rs:17`（不再构造 `RoutedFetcher`，Indexer 直接用 HTTP fetcher）, `crates/api/src/http/sites.rs`（DTO 读写 `maintenance_via`）, `crates/api/src/settings_keys.rs`, `crates/api/src/runtime_browser.rs`, `crates/api/src/http/settings.rs`（仅编译适配）
- Modify: `crates/hooks/src/plugins.rs:125`（`render: true` 随 `FetchRequest.render` 一起删除）
- Test: `crates/indexer/tests/browser_capabilities.rs`, `crates/indexer/tests/browser.rs`（删除 `RoutedFetcher` 用例）, `crates/indexer/tests/goldens.rs`, `crates/api/tests/browser_runtime.rs`（改写 `browser_runtime.rs:83` 的 `RoutedFetcher` 用例）, `crates/api/tests/config_parsing.rs`, `crates/api/tests/management/browser_settings.rs`, `crates/api/tests/management/site_credentials_review.rs`

**Interfaces:**
- Consumes: Task 1 的 `RenderedPage`、`Browser::render`、`Browser: Clone`。
- Produces:
  - **搜索永不使用 CDP**：删除 `RoutedFetcher` 和 `FetchRequest.render`。`SiteFailure` 形状不变，`engine.rs`、`fanout.rs:136`、`worker.rs:459/477` 都不用改。
  - Profile 的 `render` 字段：YAML 仍可解析（兼容已有 overlay），值为 `true` 时在加载期 `warn!(profile, "render is ignored: search never uses the Browser")`，不影响抓取；字段在下一个 minor 版本删除。
  - `pub enum MaintenanceVia { Http, Browser }`（`domain`），`Site.maintenance_via` 默认 `Http`；API 字段 `"maintenance_via": "http" | "browser"`
  - `pub enum BrowserRoute { Site, Instance, Managed }`
  - `pub struct BrowserConfig { pub enabled: bool, pub endpoint: Option<String>, pub managed_endpoint: Option<String> }`，`BrowserConfig::disabled()` 与 `Default` 等价
  - `BrowserConfig::validate_endpoint(endpoint: &str) -> Result<(), IndexerError>`
  - `BrowserConfig::resolve(&self, site: Option<&str>) -> Result<Option<(String, BrowserRoute)>, IndexerError>`；`Ok(None)` 表示开关关闭
  - `Browser::cdp_enabled(&self) -> Result<bool, IndexerError>`（读 provider，供 API 层选择 transport）
  - `settings_keys::CDP_ENABLED = "cdp.enabled"`；删除 `CDP_SYNC_ENABLED`、`OBSCURA_ENABLED`、`OBSCURA_URL`；保留 `CDP_URL`
  - `BrowserSettings { cdp_enabled: bool, cdp_url: String, user_agent: String }`，方法 `load` / `cdp_enabled()` / `cdp_url()` / `user_agent()` / `config()` / `response()` / `save`

**`resolve` 的判定顺序（实现时逐条对齐）：**
1. `!enabled` → `Ok(None)`
2. `site` 为 `Some` → 校验；空、空白或非法即 `Err`（不得静默改走实例端点）→ `Some((site, Site))`
3. `endpoint` 为 `Some` 且非空白 → 校验 → `Some((ep, Instance))`
4. `managed_endpoint` 为 `Some` 且非空白 → 校验 → `Some((ep, Managed))`
5. 否则 `Err(BrowserSetup("CDP 已开启但没有可用端点"))`

**Site `maintenance_via` 的 schema 迁移（store）：**
- `ALTER TABLE sites ADD COLUMN maintenance_via TEXT NOT NULL DEFAULT 'http' CHECK (maintenance_via IN ('http','browser'))`，按 `schema.rs` 既有的版本化迁移方式加一步。
- 迁移**一律保持 `'http'`**，不因 `cdp_url` 或 Profile `render` 改为 `'browser'`（R4-H1）。依据：旧版本生产代码的登录与签到只用 HTTP 插件——`crates/api/src/check_in.rs::run` 用 `CheckInPlugin::http`，`crates/api/src/http/sites.rs` 的 `site_login`/`site_check_in` 用 `LoginPlugin::http`/`CheckInPlugin::http`；`LoginVia::Browser` 只出现在 `hooks/tests/login_checkin.rs`、`review_remediation_login.rs`。旧的 Site `cdp_url` 与 `render` 只影响搜索渲染，而搜索渲染已删除。
- 想用浏览器登录/签到的用户需手动在 Site 上选择 `browser`；`response()` 与 Site 列表给出提示（见下）。
- Site 配置了 `proxy` 时不允许选择 `browser`（R4-L2）：CDP 会话由浏览器自己出网，不使用 Site 代理。校验写成对**写入后状态**的不变式（R5-L3）：`PUT/PATCH /sites/{id}` 合并出最终 Site 后，若 `maintenance_via == browser && proxy 非空` 即返回 400 `site.browser_proxy_unsupported`——覆盖「已有 proxy 再选 browser」与「已是 browser 再加 proxy」两个方向；同一请求里同时改回 `http` 并加 proxy 是允许的。
- `cdp_url` 的语义不变：只是该 Site 浏览器会话的端点覆盖；`maintenance_via = http` 时被忽略。

**`BrowserSettings` 的遗留迁移（写回一次，L4 + M1）：**
- 新增 `pub fn migrate_legacy(store: &Store) -> Result<bool, StoreError>`，在 `seed_store_settings` 中于启动时调用一次，单事务：
  - `cdp.enabled` 已存在 → no-op。
  - 否则 `enabled = cdp.sync.enabled || obscura.enabled`（未知取值报错，不得静默关闭）。**不再**因为 Site 配了 `cdp_url` 而打开开关（R4-H1：旧 Site 端点只服务已删除的搜索渲染）。
  - `cdp.url` 为空且 `obscura.url` 非空 → 写入 `cdp.url`。
  - 写入新 key 后删除 `cdp.sync.enabled`、`obscura.enabled`、`obscura.url`。
- `load()` 只读新 key。旧读取逻辑只存在于 `migrate_legacy`，计划在下一个 minor 版本删除它（在 `CHANGELOG`/ADR 记录这个时间点）。
- `response()` 增加（R4-L1 统一口径）：
  - `"browser_sites": n`：启用中且 `maintenance_via = browser` 的 Site 数（UI 显示）；
  - `"managed_browser_sites": n`：其中没有自己 Site `cdp_url` 的数量（Task 5 启动判定只用它）；
  - `"ignored_site_endpoints": n`：开关关闭或 `maintenance_via = http` 时配了 `cdp_url` 的 Site 数；
  - `"legacy_site_notices": [{site_id, reasons: ["cdp_url_unused" | "render_profile_ignored"], profile}]`（R4-H1/M3）：原有 `cdp_url` 已不再用于搜索、或 Profile 为 `render: true` 而搜索已改走 HTTP（可能 0 结果，需要换成可直接 HTTP 抓取的 Profile）。Site 列表 DTO 同样带 `legacy_notices` 字段，UI 在设置页与站点列表告警，并提示「如需浏览器登录/签到，请手动改为浏览器方式」。
- **存量 https 端点（R2-M2）**：Task 1 起只接受 `http`。`migrate_legacy` 和每次 `response()` 都扫描实例 `cdp.url` 与所有 Site 的 `cdp_url`，非法的（含 `https://`）**不自动改写**；迁移时 `warn!(count, "stored CDP endpoints are no longer valid")`（只记 Site id，不记 URL）；`response()` 返回 `"invalid_endpoints": [{"scope":"instance"} | {"scope":"site","site_id":n,"reason":"https_unsupported"}]`，UI 提示修改。修改前，这些 Site 的浏览器会话会在 resolve 阶段（导航前）报错，回退 HTTP。

**`response()` 形状：**
```json
{
  "cdp": {"enabled": true, "url": "", "configured": false, "route": "managed", "status": "unverified"},
  "user_agent": "crawler-media/0.1.0",
  "browser_sites": 1, "managed_browser_sites": 1, "ignored_site_endpoints": 0, "invalid_endpoints": [],
  "legacy_site_notices": [{"site_id": 3, "reasons": ["render_profile_ignored"], "profile": "foo-overlay"}],
  "capabilities": {
    "mode": "cdp-opt-in", "managed_chromium": false, "managed_obscura": true,
    "endpoint_schemes": ["http"],
    "route_precedence": ["site", "instance", "managed"],
    "uses": ["login", "check_in"], "search": "http_only",
    "fallback": "http_before_navigation", "automatic_cloudflare_fallback": false
  }
}
```
`route`：`!enabled` → `"disabled"`；`url` 空白 → `"managed"`；否则 `"instance"`。`configured` = `enabled && !url.is_empty() && validate_endpoint(url).is_ok()`。

**不得回归的既有行为：** `save` 的多 key 写入失败回滚，以及 `crates/api/tests/browser_runtime.rs` 里用 SQLite trigger 制造写失败的测试，必须保持绿；换 key 后要确认该测试覆盖的是新的 key 集合。

- [ ] **Step 1: 写 indexer 侧的失败测试**

`crates/indexer/tests/browser_capabilities.rs` 新增（用注入的 opener 记录实际端点）：

```rust
#[test] fn switch_off_resolves_to_none_even_with_a_site_endpoint() {}
#[test] fn switch_on_prefers_site_over_instance_over_managed() {}
#[test] fn switch_on_uses_instance_endpoint_when_site_is_absent() {}
#[test] fn switch_on_uses_managed_endpoint_when_nothing_is_configured() {}
#[test] fn switch_on_without_any_endpoint_is_a_setup_error() {}
#[test] fn empty_site_endpoint_is_rejected_not_silently_rerouted() {}
#[test] fn endpoint_validation_rejects_credentials_query_and_ws() {}
#[test] fn render_profile_flag_is_ignored_and_search_uses_http() {
    // render: true 的 overlay Profile；Indexer 用 RecordingFetcher 搜索 → 请求进入 HTTP fetcher，opener 从未被调用
}
```

- [ ] **Step 2: 运行并确认失败**

Run: `cargo test -p indexer --test browser_capabilities`
Expected: 编译失败（`BrowserConfig` 没有 `resolve`）。

- [ ] **Step 3: 实现 `BrowserConfig` 新形状与 `resolve`，删除死代码**

删除 `enabled`（legacy managed）、`headless`、`obscura_enabled`、`obscura_url`、`with_cdp`、`with_obscura`、`validate_managed`、`enable_in`、`chromium_present`、`chromium_path`、`RoutedFetcher`、`FetchRequest.render`。`validate_endpoint` 改为接受 `&str`，只接受 `http`（见 Task 1 Step 5）。`lib.rs` 导出 `BrowserRoute`，移除 `RoutedFetcher`；`RecordingFetcher` 保留（测试用）。

- [ ] **Step 4: 运行 indexer 测试**

Run: `cargo test -p indexer`
Expected: Step 1 用例通过；`goldens` 不回归。

- [ ] **Step 5: 写 store/api 侧迁移测试**

`crates/api/tests/config_parsing.rs` 用以下用例替换 `browser_settings_store_flag_uses_the_shared_vocabulary`：

```rust
#[test]
fn migration_maps_legacy_switches_once_and_deletes_them() {
    // 仅 cdp.sync.enabled=1 → cdp.enabled=1，旧 key 被删；再次运行为 no-op
    // 仅 obscura.enabled=1 → cdp.enabled=1；cdp.enabled=maybe → load() 报错
}
#[test]
fn legacy_site_endpoints_do_not_turn_the_switch_on_or_change_maintenance() {
    // 两个旧开关都关 + 某 Site cdp_url 非空 → cdp.enabled=0，该 Site maintenance_via = http，
    // legacy_site_notices 含 {site_id, reasons:["cdp_url_unused"]}
}
#[test]
fn legacy_obscura_url_is_adopted_as_the_instance_endpoint() {}
#[test]
fn migration_never_overrides_an_explicit_switch() {}
#[test]
fn stored_https_endpoints_are_reported_not_rewritten() {}
#[test]
fn render_overlay_sites_stay_http_and_get_a_notice() {
    // render: true overlay Site → maintenance_via = http；notices 含 render_profile_ignored 与 profile id
}
```
`crates/api/tests/management/site_credentials_review.rs` 新增 `site_maintenance_via_round_trips_and_rejects_unknown_values`、`site_with_proxy_cannot_choose_browser_maintenance`、`browser_site_cannot_add_a_proxy`、`switching_back_to_http_and_adding_a_proxy_in_one_patch_is_allowed`、`site_list_carries_legacy_notices`。

`crates/api/tests/browser_runtime.rs` 改为断言 `response()["capabilities"]["route_precedence"] == ["site","instance","managed"]` 且 `search == "http_only"`；`browser_runtime.rs:83` 的 `RoutedFetcher` 用例改为断言 Indexer 搜索从不打开 Browser。

- [ ] **Step 6: 实现 schema、`settings_keys`、`BrowserSettings`、`runtime_browser`**

同时把 `crates/api/src/http/settings.rs::sync_cdp_cookies` 的开关判断从 `CDP_SYNC_ENABLED` 改为 `CDP_ENABLED`（只为编译和语义一致；端点解析的完整改造在 Task 9）。

- [ ] **Step 7: 运行 store/api 测试**

Run: `cargo test -p store && cargo test -p api --test config_parsing --test browser_runtime --test management`
Expected: Step 5 用例通过，既有用例不回归。

- [ ] **Step 8: 全量检查后提交**

```bash
cargo check --workspace --all-targets
cargo test -p indexer && cargo test -p store && cargo test -p hooks && cargo test -p api
git add -A crates/indexer crates/domain crates/store crates/hooks crates/api
git commit -m "feat: single CDP switch for login/check-in only, per-Site maintenance_via, search stays HTTP"
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
  - `pub struct ObscuraStatus { installed, running, downloading, port, cdp_url, expected_version, reported_version, binary_path, install_policy, last_error, not_started_reason }`（`Serialize`）；`not_started_reason: Option<&'static str>` ∈ `switch_off` / `instance_endpoint_configured` / `no_managed_browser_sites`（没有需要受管 daemon 的 Site；自带 `cdp_url` 的 browser Site 不算）/ `not_installed` / `start_failed`，由 Task 5 的 reconcile 写入（R2-M3）
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
1. 已安装判定：`<data>/bin/obscura` 与 `obscura-worker` 存在，且应用自己在步骤 6 写入的 `<data>/bin/.obscura.sha256` 记录的归档哈希等于表中值 → 直接返回；预置（`PreinstalledOnly`）要求 `<data>/bin/obscura.tar.gz` 原始发布归档存在，过同样的哈希/大小校验后按步骤 5–6 解出（与 deploy 文档一致；不接受只放二进制加手写哈希文件）。否则 `PreinstalledOnly` 返回 `InstallError::NotInstalled`，**不联网**。
2. `Download`：`ureq` agent 设置 `timeout_connect=10s`、`timeout_global=300s`；若响应 `Content-Length` > `MAX_ARCHIVE_BYTES` 或 != 表中 size → 拒绝。
3. 流式写入 `<data>/bin/.obscura-<nonce>.tar.gz.part`（`create_new`），边写边 `Sha256::update`，`Read::take(MAX_ARCHIVE_BYTES + 1)` 超限即拒绝。
4. 哈希或长度不符 → 删除 `.part`、`error!(expected, actual, asset)`、返回 `InstallError::HashMismatch`。
5. 校验通过后解压到 `<data>/bin/.obscura-<nonce>/` 临时目录：逐条遍历，只接受 `EXPECTED_ENTRIES` 中的普通文件（拒绝符号链接、硬链接、`..`、绝对路径、额外条目），单条目上限 `MAX_ARCHIVE_BYTES`。
6. `chmod 0755` 后对每个文件 `fs::rename` 到 `<data>/bin/`（同一文件系统，原子）；最后写 `.obscura.sha256`。任何一步失败清理临时目录与 `.part`。
7. 修复 L3：`is_running` 中的 `.parse().unwrap()` 改为 `SocketAddr::from((Ipv4Addr::LOCALHOST, port))`。

**`start()` / 身份确认（H3 + M4）：**
- 端口：若 Task 0 证明 `--port 0` 可用且能读到实际端口（stdout 的 `CDP server: ws://127.0.0.1:<port>` 行），优先用它；否则由 `with_port_picker`（默认：绑定 `127.0.0.1:0` 取空闲端口后释放）选择。释放到 Obscura 绑定之间有竞态：就绪失败且子进程已退出时，换新端口重试至多 2 次（R2-L3），仍失败才报 `start_failed`。不再固定 9223。
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
#[test] fn port_race_retries_with_a_new_port() {}
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
  - `GET /api/v1/settings/browser/obscura` → Task 4 `ObscuraStatus` 的完整序列化（`installed, running, downloading, port, cdp_url, expected_version, reported_version, binary_path, install_policy, last_error, not_started_reason`），不另造字段集（R2-L4）
  - `POST /api/v1/settings/browser/obscura/start` → 202 `{queued:true}`（结果经状态轮询）
  - `POST /api/v1/settings/browser/obscura/stop` → `{ok:true}`
  - `GET/PUT /settings/browser` 的响应新增 `"obscura"` 字段（进程事实，来自 manager），`cdp` 段保持 Task 2 形状
  - `ApiState::with_obscura(self, Arc<ObscuraManager>) -> Self`
  - `JobKind::BrowserReconcile`（M5）与 `pub fn reconcile(manager, want: ReconcileWant) -> Result<(), String>`
  - `pub struct ReconcileWant { switch_on: bool, instance_endpoint_configured: bool, managed_browser_sites: usize }`
  - `PUT /settings/browser` 的 body 可带 `"obscura": {"install": "download" | "preinstalled"}`

**按需启动（M2，r4/r5）：** 只有 `switch_on && !instance_endpoint_configured && managed_browser_sites > 0` 才启动；`managed_browser_sites` 与 Task 2 `response()` 同一函数计算（启用中、`maintenance_via = browser`、没有自己 `cdp_url` 的 Site 数），UI 同时显示 `browser_sites` 与 `managed_browser_sites`，两者口径固定、不另算（R4-L1）。否则 `stop()`。Site 增删改（含 `maintenance_via`、`cdp_url`、`enabled` 变化）后投递一次 `BrowserReconcile`。

- [ ] **Step 1: 写失败测试**

```rust
#[tokio::test] async fn obscura_routes_are_admin_only() {}          // member → 三个路由全 403
#[tokio::test] async fn obscura_status_reports_process_facts_not_intent() {}
#[tokio::test] async fn browser_settings_response_merges_obscura_status() {}
#[tokio::test] async fn saving_the_switch_on_without_an_endpoint_starts_the_managed_daemon() {}
#[tokio::test] async fn saving_the_switch_off_stops_the_managed_daemon() {}
#[tokio::test] async fn configuring_an_instance_endpoint_stops_the_managed_daemon() {}
#[tokio::test] async fn switch_on_without_any_browser_site_does_not_start_the_daemon() {
    // 且 status.not_started_reason == "no_managed_browser_sites"
}
#[tokio::test] async fn sites_with_their_own_cdp_url_count_as_browser_but_not_managed_browser_sites() {}
#[tokio::test] async fn switching_a_site_to_browser_maintenance_queues_a_reconcile() {}
#[tokio::test] async fn preinstalled_policy_without_binary_reports_not_installed() {}
#[tokio::test] async fn obscura_status_route_matches_the_obscura_status_struct() {}
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
- `CRAWLER_MEDIA_BROWSER=1` 不再拒绝启动，**只**设置开关默认值（经 `seed_switch_default` 落成显式设置），用户已显式设置过 `cdp.enabled` 时 env 不得覆盖。它不决定安装策略，也不保证启动：在默认 `preinstalled` 且无预置二进制、或 `managed_browser_sites == 0` 时不启动，启动日志 `info!(reason = not_started_reason)` 一次，设置页显示原因（R2-M3）。`CRAWLER_MEDIA_OBSCURA_INSTALL` 同理只在 `obscura.install` 未设置时写入默认值，并在 `crates/api/src/config.rs` 注册该变量。
- 受管端点**不做**每请求 TCP 探活；只读 manager 的内存状态（H3）。端口被外部进程占用时 `cdp_url()` 为 `None`，按 Task 2 的「无可用端点」回退 HTTP 并 warn。
- `boot_reconcile` 只投递 `JobKind::BrowserReconcile`，不在启动路径上下载。
- `main.rs` shutdown 时调用 `manager.shutdown()`（M4）。
- deploy 文档注明：`network_mode: host` 下受管 Obscura 仍只绑定 `127.0.0.1`，但同宿主的其他进程可访问该端口；需要更强隔离时把 Obscura 放进独立容器/netns（后续工作）。
- `boot_reconcile` 失败只 `error!`，不阻止服务启动。

- [ ] **Step 1: 写失败测试**

```rust
#[test] fn env_flag_seeds_the_switch_only_when_it_is_unset() {}
#[test] fn env_flag_never_overrides_an_explicit_switch_value() {}
#[test] fn install_env_seeds_the_policy_only_when_unset() {}
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

`seed_store_settings` 调用 `seed_switch_default`；`create_api_state` 在 state 构建后调用 `boot_reconcile`；`production_browser(store.clone(), obscura.clone())` 构建一次，进 `ApiState`（Task 8 用）；Indexer 只用 HTTP fetcher（`RoutedFetcher` 已在 Task 2 删除）。

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
- 浏览器会话**不使用 Site 代理**（R4-L2）：`browser_page` 构造 `FetchRequest` 时不再传 `proxy`；配置了代理的 Site 在写入时已被禁止选择浏览器（Task 2），插件层再加一道防御：`site.proxy.is_some()` 时返回 `BrowserPreNavigation("browser maintenance does not use the Site proxy")`。
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
#[test] fn proxied_site_is_refused_before_navigation() {}
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
- Consumes: Task 2 的 `Site.maintenance_via`、Task 6 的 `ApiState.browser`、Task 7 的 `CheckInPlugin::browser`、既有 `site_policy`。
- Produces:
  - `pub(crate) fn login(state: &ApiState, site: &Site) -> Result<(), PluginError>`
  - `pub(crate) fn check_in(state: &ApiState, site: &Site) -> Result<(), PluginError>`

**判定与回退规则：**
1. `policy = site_policy(state, site)?`（非 NexusPHP / 无 profile 仍按原样报错）
2. `use_browser = site.maintenance_via == MaintenanceVia::Browser && state.browser.cdp_enabled()?`——读取失败（非法设置值）**不得静默当作关闭**（M7）：`error!` 并把错误返回给调用方
3. `use_browser` → 先跑 browser 插件：
   - `Err(PluginError::BrowserPreNavigation(e))` → `warn!`（含 `site_id`、`error`、`fallback = "http"`）后跑 HTTP 插件
   - 其它 `Err`（导航已发出）→ **不回退**，直接返回错误（避免重复签到/重复登录请求，H5）
4. 否则直接跑 HTTP 插件

`site_login` / `site_check_in` / `check_in::run` 三处都改为调用这两个函数，删除各自内联的插件构造。

- [ ] **Step 1: 写失败测试**

```rust
#[tokio::test] async fn browser_site_without_cdp_uses_http_login() {}
#[tokio::test] async fn browser_login_setup_failure_falls_back_to_http_and_succeeds() {}
#[tokio::test] async fn browser_check_in_setup_failure_falls_back_to_http() {}
#[tokio::test] async fn browser_check_in_failure_after_navigation_does_not_post_again() {
    // 假 Browser 在 content() 超时；断言 HTTP post 计数 == 0，返回 Err
}
#[tokio::test] async fn invalid_switch_value_is_an_error_not_http() {}
#[tokio::test] async fn http_site_never_attempts_the_browser() {}
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
git commit -m "feat(api): choose the Browser transport per Site maintenance_via and fall back to HTTP on failure"
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
- Create: `web/lib/browser-settings.ts`（纯函数：`buildBrowserSettingsBody`、`describeObscuraStatus`、`describeLegacyNotice`）
- Test: `web/test/cdp-browser-settings.test.mjs`（新建）

**Interfaces:**
- Consumes: Task 2 的 `GET/PUT /settings/browser` 形状、Task 5 的三个 Obscura 路由。
- Produces：单一开关 + 可选端点输入 + 受管 Obscura 状态区（installed/running/version/port + 安装并启动 / 停止 按钮）+ 保留 cookie 同步按钮；文案必须说明「关闭时不使用 CDP」「浏览器只用于登录与签到，搜索始终走 HTTP」「导航前失败会回退 HTTP」。

**测试模式（L5）：** 不再用源码正则断言组件文本。把请求体构造与状态文案抽成 `web/lib/browser-settings.ts` 的纯函数，用 `node --test` 直接调用（参照 `web/test/` 中已有对 `lib/` 的单测写法；若需 TS 转译，沿用仓库现有方式）。

- [ ] **Step 1: 写失败测试**

```javascript
test("body carries one switch and no legacy obscura toggle", () => {
  assert.deepEqual(buildBrowserSettingsBody({ enabled: true, url: "", install: "preinstalled" }),
    { cdp: { enabled: true, url: "" }, obscura: { install: "preinstalled" } });
});
test("status text reflects process facts, browser site count and ignored endpoints", () => {
  assert.match(describeObscuraStatus({ running: false, downloading: true, browser_sites: 2 }), /下载中/);
  assert.match(describeObscuraStatus({ running: false, not_started_reason: "no_managed_browser_sites", browser_sites: 0 }), /没有站点需要受管浏览器/);
  assert.match(describeLegacyNotice({ reasons: ["render_profile_ignored"], profile: "foo" }), /搜索改走 HTTP/);
  assert.match(describeObscuraStatus({ running: false, not_started_reason: "not_installed" }), /预置|下载/);
});
```

- [ ] **Step 2: 运行并确认失败**

Run: `cd web && node --test test/cdp-browser-settings.test.mjs`
Expected: FAIL。

- [ ] **Step 3: 改造面板**

删除独立的 Obscura 开关与 URL 输入；`cdp.url` 留空即表示使用受管 Obscura，UI 必须显式提示这一点。新增安装策略选择（仅预置 / 允许自动下载，默认仅预置）、`browser_sites` / `managed_browser_sites`、`ignored_site_endpoints`、`invalid_endpoints` 与 `legacy_site_notices` 告警（站点列表行内也显示 `legacy_notices`：「原 CDP 端点已不再用于搜索」「render Profile 已失效，搜索改走 HTTP，请更换 Profile」，并提示可手动改为浏览器登录/签到）、`not_started_reason` 与 `last_error` 显示；Site 编辑表单增加「登录/签到方式：HTTP / 浏览器」（`maintenance_via`），`cdp_url` 输入只在选择浏览器时显示；配置了代理的 Site 禁用「浏览器」选项并说明原因；文案写明「搜索始终走 HTTP」；start 返回 202 后轮询状态。

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
- 浏览器自身的网络栈不经过 net-guard。r4 起搜索不使用 Browser，插件提供的 Profile/YAML 无法触发浏览器，**不再需要**「插件 YAML 禁止 `render: true`」的约束（Task 2 中 `render` 已被忽略）。浏览器只由 Admin 在 Site 上选择 `maintenance_via = browser` 触发；若插件将来能创建 Site，插件方案须禁止其设置 `maintenance_via`/`cdp_url`（登记为插件方案待办）。
- exec-ctx 在插件方案 M0a 落地后，`Browser::render` 入口加 `debug_assert!(exec_ctx::is_blocking_ok())`；Task 1 的 Browser 信号量纳入插件方案的并发预算体系。v1 已有会话总时限（Task 1，默认 30s，含排队）与独立的清理上限（2s）；改为由调用方按 `blocking_budget` 传入 deadline 是后续工作。
- **插件方案的待办（登记，不在本计划改）**：插件不得设置 Site 的 `maintenance_via` 与 `cdp_url`；原「插件 YAML 禁止 `render: true`」一条随搜索渲染删除而撤销。
- 把受管 Obscura 放进受限网络（容器 / netns）列为后续工作。

## 收尾

- [ ] `cargo check --workspace --all-targets` 与 `cargo test --workspace` 全绿。
- [ ] `cd web && node --test && pnpm typecheck` 全绿。
- [ ] **手动验收（有网络的机器，结果写进 ADR）**：设 `obscura.install=download` → 开关开启 → 下载并哈希校验 → 启动（确认仅监听 127.0.0.1、端口随机）→ 把一个 Site 设为 `maintenance_via = browser` → 确认搜索仍走 HTTP（日志无 Browser 会话）→ 确认升级前带 `cdp_url`/render 的 Site 仍为 `http` 且设置页与站点列表有提示→ 浏览器登录与签到（同时触发两个不同站点，确认两个 tab 并发且 cookie 互不干扰；同时触发两个兄弟子域站点，确认串行；确认 `path=/forum` 的 cookie 与父域 cookie 在会话后被删除；把排队上限调小，确认剩余时间不足时在导航前返回 busy 并回退 HTTP） → cookie 回读并合并 → 关闭开关后进程退出、pidfile 删除。
- [ ] 按 Review Focus 十条逐条确认对应测试存在且真的在断言该行为。
- [x] （已在 Task −1 完成，收尾只复核）ADR-0007 与 `docs/deploy.md`：信任根是写死的 SHA-256（Task 4）；离线预置**只需**放原始发布归档 `<data>/bin/obscura.tar.gz`，应用校验哈希后自行解出（`.obscura.sha256` 是应用写入的安装标记，运维无需也不应手写）；`obscura.install` 默认 `preinstalled`；`network_mode: host` 风险；端点仅 `http`；Browser 只用于登录/签到。
- [ ] 确认 `docs/adr/0007-cdp-browser-opt-in.md`、`CONTEXT.md`、`docs/deploy.md` 的描述与最终实现一致；如有偏差，改代码或改文档，不允许两边不一致。
- [ ] 在 worktree 提交完整分支，`main` 保持干净。
