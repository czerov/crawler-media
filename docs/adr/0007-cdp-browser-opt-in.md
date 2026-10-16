# Browser 走 CDP：单一开关、仅用于登录与签到、受管 Obscura 兜底、导航前失败回退 HTTP

**Browser**（CDP 会话）只用于 Site 的登录与 **Check-in**，二者是该会话上的第一方插件。**搜索与 RSS 永远走 HTTP，不使用 CDP**（2026-10-16 r4 范围调整：删除 Profile `render` 搜索路径）。默认 Docker 镜像不打包 Chromium 或 Obscura；受管 Obscura 默认只用预置二进制，Admin 显式允许后才下载。这不是 Windows RDP。

## 决策（2026-10-16，r2 按计划修订同步）

取代 2026-10-10 的 **external-CDP-only** 边界（见文末「历史边界」）。实现计划：`docs/superpowers/plans/2026-10-16-cdp-obscura-managed-browser.md`。

### 单一开关

实例级只有一个 Browser/CDP 开关 `cdp.enabled`，取代原先并列的 `cdp.sync.enabled` 与 `obscura.enabled`。

- **关闭**：完全不使用 CDP。登录与 **Check-in** 走 HTTP 插件。不探测端点、不下载、不启动任何浏览器进程。Site 上的 `cdp_url` 在开关关闭时**不生效**；`/settings/browser` 返回被忽略的数量 `ignored_site_endpoints`，UI 告警。
- **开启**：`maintenance_via = browser` 的 Site 用浏览器登录与签到，端点按下一节的顺序解析；`maintenance_via = http`（默认）的 Site 仍走 HTTP。
- **搜索不受开关影响**：始终 HTTP。Profile 的 `render` 字段被忽略（加载时 `warn!`），下一个 minor 版本删除。
- 开关读取失败（非法值）报错，**不得**静默当作关闭。

### 端点解析（开关开启时）

1. **Site 显式 `cdp_url`** —— 最具体的用户配置，优先；空或非法即报错，不静默改走下一级。
2. **实例级 `cdp.url`**。
3. **受管 Obscura** —— 仅当本进程持有存活且身份已确认的受管 daemon 时才作为端点；端口被外部进程占用时不视为受管端点，按「无可用端点」回退 HTTP。

配置了连接的端点失败时**不**回退到 Obscura，直接按下一节处理。

### 失败回退 HTTP（分阶段）

- **登录 / Check-in**：只有**导航之前**的失败（端点不可用〔CDP 关闭或无可用端点〕、排队超时 `BrowserBusy`、target / WebSocket 建立、cookie 注入失败——服务端尚未收到请求）才回退 HTTP，并 `warn!`（Site、route、原因）。**端点配置非法**（设置读取失败、URL 不合法等）不回退，直接报错，以便尽早暴露配置问题。**导航已发出之后**的失败不回退，直接报错，避免重复签到或重复登录请求。
- 回退不等于成功；HTTP 遇到质询照常报错，不重试浏览器路径。
- 单个浏览器会话有总时限（默认 30s，可配置，含排队），各步骤共用剩余时间；排队另有独立上限（默认 15s），拿到名额后剩余时间不足 10s 即在导航前返回 busy（可回退 HTTP）。关闭 CDP target 有独立上限（默认 2s），超时只 `warn!`，单会话最多额外占用该清理上限。cookie 不做 session-end 清理。

### 并发与 cookie 隔离

- **多标签并发**：同一 CDP endpoint 的 tab 共用 cookie jar；实例全局并发上限默认 3（1–8 可配），同一可注册域串行以避免同一 cookie 范围内的并发覆盖。外部/第三方 Browser 的 cookie jar 不视为应用私有数据。
- **会话隔离而非删除**（r9）：不调用 `Network.deleteCookies`、`Network.clearBrowserCookies` 或 `Storage.clearCookies`；每个会话在独立 BrowserContext 中运行，结束即 dispose，第三方 Browser 默认上下文的 cookie 不受影响，会话 cookie 不跨会话残留。
- **host-only 注入与定向回读**：Site 已配置的 cookie 只经 `Network.setCookies` 使用当前 Site URL 做 host-only 注入；不得使用 `Network.setExtraHTTPHeaders`（会发给第三方主机）。读取 cookie 只发 `Network.getCookies{urls:[当前 Site URL]}`，结果按 name 去重后合并回该 Site 的 cookie 字段；不读取或回写无关域的 cookie。
- **host 规范化**：Site URL host 用于 cookie host-only 注入与并发分组时统一小写、去尾点、IDNA 转 ASCII、IPv6 去方括号并规范化；不遍历或归类 cookie domain。
- **公共后缀 cookie 前提**：Task 0 实测 Obscura 是否拒收公共后缀级 cookie（如 `Domain=.co.uk`、`.github.io`）。若该 endpoint 接受此类 cookie，视为 cookie 隔离不兼容，不用它执行 Browser maintenance；不以删除全局 cookie 补救。
- IP 地址规范化为标准写法（如 `2001:db8::1`），按整个 IP 分组。
- **跳转 host**：不扫描、不回写、不删除跳转到的 SSO/质询 host cookie；`cookie_header` 只读取当前 Site URL 可见的 cookie，因此跨 host SSO 的认证状态不保证合并回 Site。
- **有界 FIFO 排队**：拿不到全局名额或分组时按票号先来先服务排队，超过排队上限或剩余时间不足即返回 busy（导航前失败，可回退 HTTP）。
- 运行前提仅为 `Network.setCookies` 和 `Network.getCookies{urls}`。不要求 `Network.getAllCookies` / `Storage.getCookies` / `Network.deleteCookies`；浏览器 target 关闭不改变 cookie jar。

### 受管 Obscura 生命周期

**启动条件**（全部满足）：开关开启、未配置 `cdp.url`、至少一个启用中、`maintenance_via = browser` 且没有自己 Site `cdp_url` 的 Site（`managed_browser_sites > 0`；设置页另显示含自带端点的 `browser_sites`）、二进制已安装（或安装策略允许下载）。默认所有 Site 为 `http`，因此只开开关不会启动浏览器进程。不满足时 `/settings/browser` 返回未启动原因（`switch_off` / `instance_endpoint_configured` / `no_managed_browser_sites` / `not_installed` / `start_failed`）。Site 的 `maintenance_via`、`cdp_url`、启用状态变化后重新评估。

- **浏览器会话不使用 Site 代理**（浏览器自己出网）。不变式：`maintenance_via = browser` 的 Site 不能有 `proxy`——无论先设哪一项，写入后违反即 400，以免直连暴露真实出口或因 IP 不一致失败。
- **安装策略** `obscura.install`：默认 `preinstalled`（只用预置二进制，不联网）；Admin 显式改为 `download` 才会从发布地址下载。启动默认值可由 `CRAWLER_MEDIA_OBSCURA_INSTALL` 设置。
- **端口与绑定**：每次启动选随机空闲端口，并以 `--host 127.0.0.1` 绑定回环（以 Task 0 实测为准；无法保证回环绑定则拒绝启用受管模式）。就绪与身份以 `/json/version` 加本进程子进程存活判定。端口竞争导致启动失败时换端口重试至多 2 次。
- **进程身份**：pidfile `<data>/run/obscura.pid`（pid、端口、nonce）。启动时只回收 exe 指向 `<data>/bin/obscura` 的旧进程；其它进程只删 pidfile，不杀。应用正常退出时停止受管 daemon。
- 安装与启动在 Job（`BrowserReconcile`）中执行，保存设置不等待下载。下载或启动失败记 `error!` 并写入状态 `last_error`；开关保持开启，请求按上节回退。
- 版本以 `/json/version` 上报为准，代码常量只是期望值，不一致 `warn!`。

### 信任模型（受管二进制）

- 信任根是**代码中写死的各平台资产 SHA-256 与大小**，随 `OBSCURA_VERSION`（当前 `v0.2.3`）一起更新。我们信任的是「这几个具体文件」，不是 `h4ckf0r0day/obscura` 仓库本身——该仓库为第三方维护、自述为防检测浏览器，上游不提供签名。
- 下载先写临时文件，边写边计算哈希；大小上限 200 MiB，`Content-Length` 必须与表一致；不符即删除并报错。归档只接受 `obscura`、`obscura-worker` 两个普通文件，校验通过后原子 rename 到 `<data>/bin/`。
- 预置二进制同样必须通过哈希校验（见 deploy 文档的预置规则）。
- 升级版本：人工下载、核对哈希、改表、重新跑计划 Task 0 兼容性检查。

### 端点与协议约束

- 只接受 **`http` CDP discovery URL**（`/json/version`、`/json/new`）；不接受 `https`（当前 transport 只支持 `ws`，`wss` 为后续工作），也不接受直接 `ws`/`wss` 调试 WebSocket。
- discovery 返回的 `webSocketDebuggerUrl` 的 host:port 必须与端点一致。
- 端点不接受内嵌凭据与 query string；空/非法端点写入时 400 拒绝，不静默 fallback。
- 启动构造与每次真实请求共用同一配置入口：API 以 provider closure 从 Store 读取实时设置，Indexer 不依赖 API/Store。保存后立即生效。
- 设置写失败返回非 200 并恢复先前配置。保存只表示配置成功，不做联网连通性检查。
- 浏览器只能由 Admin 在 Site 上选择 `maintenance_via = browser` 触发；Profile（含插件提供的 YAML）无法触发浏览器。浏览器网络栈不经过 net-guard，若插件将来能创建 Site，插件方案须禁止其设置 `maintenance_via`/`cdp_url`。

### Fresh-start 数据边界（不做兼容迁移）

- （r9）存量数据目录原地增量升级，存量 Site 维护方式为 `http`；应用**不得自动删除**用户数据。
- 新建数据库的 `sites` 基表包含 `maintenance_via TEXT NOT NULL DEFAULT 'http'`；所有新 Site 默认 HTTP。新设置只读写 `cdp.enabled`、`cdp.url` 与 `obscura.install`，不读取、不迁移、不兼容 `cdp.sync.enabled`、`obscura.enabled`、`obscura.url`。
- 旧 schema/旧数据目录不受支持。启动时应在任何 schema 写入前明确报错并提示删除旧数据目录；不自动 ALTER/backfill 旧 Site、不自动改变 `maintenance_via`，不因旧 `cdp_url` 或 Profile `render: true` 自动开启 CDP。
- Profile 中遗留的 `render: true` 不再控制搜索，加载时忽略并 `warn!`；不创建旧 Site 的 `legacy_site_notices`、旧端点扫描或兼容迁移 API。
- 新写入的实例/Site 端点只接受 `http`；内嵌凭据、query、`https`、`ws`/`wss` 在写入时 400 拒绝。无需扫描或修复旧端点，因为旧数据目录不受支持。

### 凭据

- 浏览器会话读回的 cookie 是真实站点凭证：只持久化 CDP `Network.getCookies{urls:[当前页]}` 实际返回的值，按 name 去重。**绝不合成**占位会话值（历史上 `PageSession::cookie_header()` 默认返回 `session=browser` 属于伪造凭证，必须移除）。
- 读不到 cookie 时保留原有 cookie，不写入空值或占位值。
- cookie 明文不得进日志与前端「系统日志」。

### 部署

- 默认镜像仍不带 Chromium，也不带 Obscura。离线或默认（`preinstalled`）部署须按 deploy 文档预置二进制，或配置外部 `cdp.url`。
- `CRAWLER_MEDIA_BROWSER` 只设置开关的启动默认值（仅在 `cdp.enabled` 未设置时写入），不再导致启动失败；它**不**决定是否下载，也不保证启动浏览器（见上方启动条件）。

## r9 修订（2026-10-16，取代 r8 中的 fresh-start 与"不删 cookie"）
- **数据升级**：不要求空数据目录。v7 库增量补 `sites.maintenance_via`（默认 `http`），已有数据全部保留。
- **会话隔离**：每个 Browser 会话使用独立的 BrowserContext（`Target.createBrowserContext` → `createTarget{browserContextId}` → `disposeBrowserContext`）。本应用不调用任何 cookie 删除命令，也不会碰到其他上下文里的 cookie；会话结束后，该会话的 cookie 随上下文一起销毁，因此不会串号，也不会残留凭据。不支持 BrowserContext 的端点标记为不兼容，登录/签到在导航前回退 HTTP。
- **回退边界**：只有导航前失败（BrowserSetup/BrowserBusy）才回退 HTTP，且回退时不重复发 Hook 事件；导航后的失败原样报告。
- **T0 验证表**：2026-10-16 在 box 上用 Obscura v0.2.3（x86_64-linux，sha256 `1534d1e6…eec` 校验通过）实测。前 6 项结果出自本仓库测试（`crates/indexer/tests/obscura_t0.rs`、`crates/api/tests/management/obscura_lifecycle.rs`，均为 `#[ignore]`），其余出自复审 r2 的 Node 探针：

| 能力 | 结果 |
|---|---|
| `/json/version` | 支持；`Browser` 字段报 `Chrome/145.0.0.0`，所以版本改用 `obscura --version` 核对（输出 `obscura 0.2.3`） |
| `/json/new`、`/json/close` | **不支持**，已改为 `Target.createTarget` / `Target.closeTarget` |
| `createBrowserContext` / `createTarget{browserContextId}` / `disposeBrowserContext` | 支持；第二个会话读不到上一个会话的 cookie |
| 直连 `/devtools/page/{id}` 后发 `Page.navigate` | `-32601 No page for session`，已改为 `Target.attachToTarget{flatten:true}` 加 sessionId |
| 端到端 render（注入 cookie、goto、读取 content、回读 `getCookies{urls}`） | **通过**（`render_end_to_end_navigates_injects_and_reads_back_cookies`） |
| 受管 start：spawn 线程退出后 daemon 仍存活，stop 后进程退出 | **通过**（`real_obscura_managed_start_survives_spawn_thread_and_stops`） |
| `getCookies{urls}` 是否按 path 过滤 | **不过滤**，所以回读只取 path=`/` 的 cookie |
| 访问私网目标 | 默认禁止；需要的话设置 `CRAWLER_MEDIA_OBSCURA_ALLOW_PRIVATE_NETWORK=1`，启动时会加上 `--allow-private-network` |
| 拒收公共后缀 cookie | 未测 |

## 历史边界（2026-10-10，已被上一节取代）

> 以下为被取代的边界记录，保留用于追溯。**旧文档的日期与绿测不表示当前能力。**

原生产能力为 **external-CDP-only**：仓库没有可靠的受管 Chromium 下载/启动实现；`BrowserConfig::enable_in` 创建的空目录及 HEADLESS 文件不能证明存在 Chromium，因此 `enable_in` 与 `CRAWLER_MEDIA_BROWSER=1` 明确拒绝受管能力，不创建占位目录。Obscura 的下载/启动辅助器当时只证明可以尝试启动第三方 daemon，Browser 设置及开机不再自动调用它；用户须自行准备运行中的兼容外部服务。当时的路由优先级为 Site `cdp_url` → 启用的 Obscura → 启用的全局 CDP，且显式 Site 端点在全局开关关闭时仍可使用。

上述「受管能力不可用、不自动调用 Obscura、Site 端点凌驾开关」三点均被 2026-10-16 决策取代。

## 验证边界

回归测试注入 PageSession/opener 与受管生命周期 seam（installer/launcher），断言路由选择、回退确实发生、以及回退时请求确实走了 HTTP。不下载或启动真实 Obscura，不启动 Chromium，不访问实网（localhost 假 CDP 服务是允许的，仓库已有先例）。

**Obscura 实测兼容性**：待计划 Task 0 填写（方法 → 支持/不支持 → 降级方式；监听地址；`--port 0`；浏览器 GET 签到是否生效）。在此之前，受管 Obscura 的 CDP 兼容性**未经验证**。

## 理由与备选方案

默认打包 Chromium 会让每个只需要 HTTP 的 NAS 部署膨胀；完全不支持 CDP 会让需要浏览器才能通过的登录/签到无法自动化。在已预置（或允许下载）二进制、且确有 Site 选择浏览器登录/签到时，受管 Obscura 兜底让「开关打开即可用」成立；导航前失败回退 HTTP 保证浏览器链路故障不会让签到整体失效。搜索不走浏览器：内置 Profile 全部不需要渲染，渲染搜索带来的并发、降级可见性与资源成本不值得在 v1 承担。单一开关避免用户面对「开了全局 CDP 但没配 URL」这类无意义组合。

**备选方案**：默认自动下载（被否决：开关一开就执行第三方程序，供应链风险由用户被动承担）；镜像始终带 Chromium；保留两个独立开关；失败即报错而不回退；端点失败后改用 Obscura 重试（被否决：会让故障表现为不可预期的第二跳延迟，且与「用户配置的连接优先」相矛盾）。
