# Browser 走 CDP：单一开关、受管 Obscura 兜底、失败回退 HTTP

JS 渲染的搜索在 CDP 支撑的 **Browser** 抓取后（`render: true`）使用同一套 Indexer 选择器。登录与 **Check-in** 是该会话上的第一方插件。默认 Docker 镜像不打包 Chromium 或 Obscura；受管 Obscura 默认只用预置二进制，Admin 显式允许后才下载。这不是 Windows RDP。

## 决策（2026-10-16，r2 按计划修订同步）

取代 2026-10-10 的 **external-CDP-only** 边界（见文末「历史边界」）。实现计划：`docs/superpowers/plans/2026-10-16-cdp-obscura-managed-browser.md`。

### 单一开关

实例级只有一个 Browser/CDP 开关 `cdp.enabled`，取代原先并列的 `cdp.sync.enabled` 与 `obscura.enabled`。

- **关闭**：完全不使用 CDP。所有抓取走 HTTP；登录与 **Check-in** 走 HTTP 插件。不探测端点、不下载、不启动任何浏览器进程。Site 上的 `cdp_url` 在开关关闭时**不生效**；`/settings/browser` 返回被忽略的数量 `ignored_site_endpoints`，UI 告警。
- **开启**：启用浏览器链路，端点按下一节的顺序解析。
- 开关读取失败（非法值）报错，**不得**静默当作关闭。

### 端点解析（开关开启时）

1. **Site 显式 `cdp_url`** —— 最具体的用户配置，优先；空或非法即报错，不静默改走下一级。
2. **实例级 `cdp.url`**。
3. **受管 Obscura** —— 仅当本进程持有存活且身份已确认的受管 daemon 时才作为端点；端口被外部进程占用时不视为受管端点，按「无可用端点」回退 HTTP。

配置了连接的端点失败时**不**回退到 Obscura，直接按下一节处理。

### 失败回退 HTTP（分阶段）

- **render 搜索**：任何 CDP 环节失败（含并发已满 `BrowserBusy`、总时限超时）都回退 HTTP 并 `warn!`（Site、route、原因）。结果来源记为 `HttpFallback`；回退后解析得到 0 个候选时记为 `SiteFailure{kind: CdpFallbackEmpty}`，订阅与 UI 可见，不伪装为「站点没有资源」。
- **登录 / Check-in**：只有**导航之前**的失败（端点解析、并发已满、target / WebSocket 建立、cookie 注入失败——服务端尚未收到请求）才回退 HTTP。**导航已发出之后**的失败不回退，直接报错，避免重复签到或重复登录请求。
- 回退不等于成功；HTTP 遇到质询照常报错，不重试浏览器路径。
- 单次 render 有总时限（默认 15s，可配置），各步骤共用剩余时间。

### 并发与 cookie 隔离

- 站点 cookie 只经 `Network.setCookies` 按站点 host 注入（host-only，https 置 `secure`），**不得**使用 `Network.setExtraHTTPHeaders`（会发给第三方主机）。
- 每会话使用独立浏览上下文（`Target.createBrowserContext`）时，并发上限默认 2。Obscura 不支持或未经验证支持时，会话结束只能 `Network.clearBrowserCookies` 清理共享 cookie jar，此时**代码强制并发上限为 1**，避免一个会话结束时清掉另一会话的 cookie。能力以计划 Task 0 的实测为准。
- 拿不到并发许可立即失败（busy），不阻塞调用方。

### 受管 Obscura 生命周期

**启动条件**（全部满足）：开关开启、未配置 `cdp.url`、至少一个启用中的 Site 使用 `render: true` 的 Profile（`render_sites > 0`）、二进制已安装（或安装策略允许下载）。内置 Profile 当前全部 `render: false`，因此只开开关不会启动浏览器进程。不满足时 `/settings/browser` 返回未启动原因（`switch_off` / `instance_endpoint_configured` / `no_render_sites` / `not_installed` / `start_failed`）。

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
- `render` 只对内置与 Admin 本地 overlay Profile 开放；插件来源 Profile 禁止 `render: true`（浏览器网络栈不经过 net-guard）。

### 迁移规则（一次性，启动时）

- `cdp.enabled` 已存在 → 不迁移。
- 否则 `cdp.enabled = cdp.sync.enabled || obscura.enabled`；若两者都关但有 Site 配了非空 `cdp_url`（旧边界允许「全局关 + Site 端点」）→ 置 `cdp.enabled=1` 并记 `info!`，避免升级后静默失去渲染。
- `cdp.url` 为空且 `obscura.url` 非空 → 写入 `cdp.url`。写入新 key 后删除三个旧 key；迁移代码在下一个 minor 版本删除。
- 已保存的 `https://` 实例或 Site 端点在新规则下非法：**不自动改写**，记 `warn!`，并在 `/settings/browser` 的 `invalid_endpoints` 中列出，由 UI 提示用户修改；在修改前这些端点的 render 请求按失败回退 HTTP。

### 凭据

- 浏览器会话读回的 cookie 是真实站点凭证：只持久化 CDP `Network.getCookies{urls:[当前页]}` 实际返回的值，按 name 去重。**绝不合成**占位会话值（历史上 `PageSession::cookie_header()` 默认返回 `session=browser` 属于伪造凭证，必须移除）。
- 读不到 cookie 时保留原有 cookie，不写入空值或占位值。
- cookie 明文不得进日志与前端「系统日志」。

### 部署

- 默认镜像仍不带 Chromium，也不带 Obscura。离线或默认（`preinstalled`）部署须按 deploy 文档预置二进制，或配置外部 `cdp.url`。
- `CRAWLER_MEDIA_BROWSER` 只设置开关的启动默认值（仅在 `cdp.enabled` 未设置时写入），不再导致启动失败；它**不**决定是否下载，也不保证启动浏览器（见上方启动条件）。

## 历史边界（2026-10-10，已被上一节取代）

> 以下为被取代的边界记录，保留用于追溯。**旧文档的日期与绿测不表示当前能力。**

原生产能力为 **external-CDP-only**：仓库没有可靠的受管 Chromium 下载/启动实现；`BrowserConfig::enable_in` 创建的空目录及 HEADLESS 文件不能证明存在 Chromium，因此 `enable_in` 与 `CRAWLER_MEDIA_BROWSER=1` 明确拒绝受管能力，不创建占位目录。Obscura 的下载/启动辅助器当时只证明可以尝试启动第三方 daemon，Browser 设置及开机不再自动调用它；用户须自行准备运行中的兼容外部服务。当时的路由优先级为 Site `cdp_url` → 启用的 Obscura → 启用的全局 CDP，且显式 Site 端点在全局开关关闭时仍可使用。

上述「受管能力不可用、不自动调用 Obscura、Site 端点凌驾开关」三点均被 2026-10-16 决策取代。

## 验证边界

回归测试注入 PageSession/opener 与受管生命周期 seam（installer/launcher），断言路由选择、回退确实发生、以及回退时请求确实走了 HTTP。不下载或启动真实 Obscura，不启动 Chromium，不访问实网（localhost 假 CDP 服务是允许的，仓库已有先例）。

**Obscura 实测兼容性**：待计划 Task 0 填写（方法 → 支持/不支持 → 降级方式；监听地址；`--port 0`；浏览器 GET 签到是否生效）。在此之前，受管 Obscura 的 CDP 兼容性**未经验证**。

## 理由与备选方案

默认打包 Chromium 会让每个只需要 HTTP 的 NAS 部署膨胀；完全不支持 CDP 会丢掉需要 JS 渲染与人工认证的 tracker。受管 Obscura 兜底让「开关打开即可用」成立，而失败回退 HTTP 保证浏览器链路故障不会让抓取与签到整体失效。单一开关避免用户面对「开了全局 CDP 但没配 URL」这类无意义组合。

**备选方案**：默认自动下载（被否决：开关一开就执行第三方程序，供应链风险由用户被动承担）；镜像始终带 Chromium；保留两个独立开关；失败即报错而不回退；端点失败后改用 Obscura 重试（被否决：会让故障表现为不可预期的第二跳延迟，且与「用户配置的连接优先」相矛盾）。
