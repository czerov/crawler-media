# Browser 走 CDP：单一开关、受管 Obscura 兜底、失败回退 HTTP

JS 渲染的搜索在 CDP 支撑的 **Browser** 抓取后（`render: true`）使用同一套 Indexer 选择器。登录与 **Check-in** 是该会话上的第一方插件。默认 Docker 镜像仍不打包 Chromium，受管 Obscura 按需下载。这不是 Windows RDP。

## 决策（2026-10-16）

取代 2026-10-10 的 **external-CDP-only** 边界（见文末「历史边界」）。

### 单一开关

实例级只有一个 Browser/CDP 开关，取代原先并列的 `cdp.sync.enabled` 与 `obscura.enabled`。

- **关闭**：完全不使用 CDP。所有抓取走 HTTP；登录与 **Check-in** 走 HTTP 插件。不探测端点、不下载、不启动任何浏览器进程。Site 上的 `cdp_url` 在开关关闭时**不生效**（仅记日志说明被忽略），因为「没开 CDP 就不需要 CDP」是用户的显式选择，不是可被 per-Site 配置绕过的默认值。
- **开启**：启用浏览器链路，端点按下一节的顺序解析。

### 端点解析（开关开启时）

1. **Site 显式 `cdp_url`** —— 最具体的用户配置，优先。
2. **实例级 `cdp.url`** —— 用户配置的连接。
3. **受管 Obscura** —— 未配置 `cdp.url` 时自动生效，端点即本地受管 daemon。

配置了连接的端点失败时**不**回退到 Obscura：按下一节直接回退到 HTTP。Obscura 只是「用户没有指定端点」时的兜底，不是端点失败后的第二选择。

### 失败回退 HTTP

任何 CDP 环节失败——端点不可达、target 建立失败、WebSocket 握手失败、导航失败、内容读取失败、超时——都回退到**不使用 CDP 的 HTTP 流程**，并记 `warn!`（含 Site、route、失败原因）。回退覆盖全部调用方：render 搜索、登录、**Check-in**。

边界与风险，必须被日志区分，不能伪装成成功：

- render 搜索回退 HTTP 后可能拿不到 JS 渲染内容，解析得到 0 个候选或解析失败——这是显式降级，不是「CDP 成功」。日志必须能区分「CDP 失败已回退 HTTP」与「HTTP 本身失败」。
- 登录/签到回退 HTTP 时，若站点有 Cloudflare/验证码质询，HTTP 会被质询拦截并报错。回退不等于成功，也不重试浏览器路径。
- 回退是降级而非静默：没有 CDP 可用性证据时，不得向调用者宣称走了浏览器。

### 受管 Obscura 生命周期

开关开启且未配置 `cdp.url` 时，应用负责 Obscura 的完整生命周期：

- 缺失则下载并安装到数据目录（`<data>/bin`），然后启动 daemon 并监听本地端口。
- 提供安装/启动/停止/状态的显式操作与状态上报，状态必须区分**配置意图**（开关）、**进程事实**（installed / running / downloading / port / version）与**可用性验证**（尚未建立）。
- 只管理本进程拉起的 daemon。同端口上已存在的外部服务只探测、不接管；开关关闭时只停止本进程拉起的进程，绝不杀外部服务。
- 下载或启动失败记 `error!`；开关保持开启但链路不可用，后续请求按「失败回退 HTTP」处理。
- Obscura 必须提供兼容的 HTTP(S) CDP discovery（`/json/version`、`/json/new`）。不兼容即视为 CDP 失败并回退。

### 端点与协议约束

- 只接受 **HTTP(S) CDP discovery URL**；不支持直接 `ws`/`wss` 调试 WebSocket。
- 端点不接受内嵌凭据（userinfo）与 query string；空/非法端点明确报错，不静默 fallback 到其它端点。
- 启动构造与每次真实请求共用同一配置入口：API 以 provider closure 从 Store 读取实时设置，Indexer 不依赖 API/Store。保存后后续请求立即生效，重启选择保持一致。
- 设置写失败返回非 200 并尝试恢复先前配置，不吞掉持久化失败。保存只表示配置成功，不进行联网连通性检查。

### 凭据

- 浏览器会话读回的 cookie 是真实站点凭证：只持久化 CDP `Network.getCookies` 实际返回的值。**绝不合成**任何占位会话值（历史上 `PageSession::cookie_header()` 默认返回 `session=browser` 属于伪造凭证，必须移除）。
- 读不到 cookie 时按「无新凭证」处理，保留原有 cookie，不写入空值或占位值。
- cookie 明文不得进日志与前端「系统日志」（沿用现有 `Debug` redaction）。

### 部署

- 默认镜像仍不带 Chromium。受管 Obscura 二进制由应用按需下载到数据目录，首次使用需要能访问其发布地址；离线部署须预置二进制或改为配置外部 `cdp.url`。
- `CRAWLER_MEDIA_BROWSER` 从「拒绝启动的旧 managed 开关」改为启动期 Browser 开关默认值；置位不再导致启动失败。

## 历史边界（2026-10-10，已被上一节取代）

> 以下为被取代的边界记录，保留用于追溯。**旧文档的日期与绿测不表示当前能力。**

原生产能力为 **external-CDP-only**：仓库没有可靠的受管 Chromium 下载/启动实现；`BrowserConfig::enable_in` 创建的空目录及 HEADLESS 文件不能证明存在 Chromium，因此 `enable_in` 与 `CRAWLER_MEDIA_BROWSER=1` 明确拒绝受管能力，不创建占位目录。Obscura 的下载/启动辅助器当时只证明可以尝试启动第三方 daemon，Browser 设置及开机不再自动调用它；用户须自行准备运行中的兼容外部服务。当时的路由优先级为 Site `cdp_url` → 启用的 Obscura → 启用的全局 CDP，且显式 Site 端点在全局开关关闭时仍可使用。

上述「受管能力不可用、不自动调用 Obscura、Site 端点凌驾开关」三点均被 2026-10-16 决策取代。

## 验证边界

回归测试注入 PageSession/opener 与受管生命周期 seam（installer/launcher），断言路由选择、回退确实发生、以及回退时请求确实走了 HTTP。不下载或启动真实 Obscura，不启动 Chromium，不访问实网（localhost 假 CDP 服务是允许的，仓库已有先例）。

## 理由与备选方案

默认打包 Chromium 会让每个只需要 HTTP 的 NAS 部署膨胀；完全不支持 CDP 会丢掉需要 JS 渲染与人工认证的 tracker。受管 Obscura 兜底让「开关打开即可用」成立，而失败回退 HTTP 保证浏览器链路故障不会让抓取与签到整体失效。单一开关避免用户面对「开了全局 CDP 但没配 URL」这类无意义组合。

**备选方案**：镜像始终带 Chromium；保留两个独立开关；失败即报错而不回退；端点失败后改用 Obscura 重试（被否决：会让故障表现为不可预期的第二跳延迟，且与「用户配置的连接优先」相矛盾）。
