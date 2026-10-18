# Deploy

API + Jellyfin 兼容服务是单个 Rust 二进制（`crates/api`）。前端是 Vite + React SPA（`web/`，pnpm）。

## 部署形态

- **开发**：两个端口。Vite dev（3334）经 `VITE_API_BASE_URL` 直连后端（18765），
  跨域由后端 CORS 层放行（`CRAWLER_MEDIA_CORS_ORIGINS`，默认放行 127.0.0.1:3334 / localhost:3334）。
- **生产**：一个端口。Rust 监听 `CRAWLER_MEDIA_LISTEN`（默认 127.0.0.1:18765），同时托管：
  - `/` —— 前端静态产物（`CRAWLER_MEDIA_UI` 指向 `web/dist`，SPA fallback 到 index.html）
  - `/api/v1/*` —— 自有 API
  - Jellyfin 兼容路径（/Items、/Videos/...、/Sessions/...）—— 播放器直连同一端口

## 本地运行

```bash
# 1. 后端（数据目录默认 ./data，可用 CRAWLER_MEDIA_DATA 覆盖）
cargo run -p api

# 2. 前端（开发模式，直连后端，跨域走后端 CORS）
cd web
VITE_API_BASE_URL=http://127.0.0.1:18765/api/v1 pnpm dev   # 监听 127.0.0.1:3334

# 3. 生产构建 + 单端口冒烟
cd web
pnpm build                       # 产出 dist/
CRAWLER_MEDIA_UI=$PWD/dist cargo run -p api   # 浏览器打开 http://127.0.0.1:18765 即完整应用
```

默认监听：后端 `127.0.0.1:18765`，前端 `127.0.0.1:3334`。首次创建管理员时须独立设置 `CRAWLER_MEDIA_ADMIN_PASSWORD`；已有管理员使用当前保存的口令，不能把该启动变量当作每次重启都会覆盖口令。CLI token 与网页登录口令独立。

构建要求：rustc ≥ 1.88（`idna`/`icu` 依赖要求；`Dockerfile` 使用 `rust:bookworm`，自建镜像需确认版本）。

## Docker Compose

```bash
export CRAWLER_MEDIA_TOKEN=your-cli-bearer-token
export CRAWLER_MEDIA_ADMIN_PASSWORD=your-distinct-admin-password
docker compose up --build
```

镜像内单进程（`crawler-media`）监听 18765：静态 UI（`web/dist` 拷入
`/usr/share/crawler-media/ui`）+ API + Jellyfin 兼容，浏览器和播放器都打这一个端口。
镜像内默认 **无 Chromium**。实例级 Browser 开关决定是否使用 CDP：**关闭**时所有请求走 HTTP，不探测端点、不下载、不启动任何浏览器进程，Site 的 `cdp_url` 也不生效；**开启**时端点按 Site `cdp_url` → 实例 `cdp.url` → 受管 Obscura 解析，浏览器只用于 Site 的登录与签到（Site 上选择「浏览器」方式，`maintenance_via = browser`）；**搜索与 RSS 始终走 HTTP**。失败回退规则见下段。端点只接受 `http` CDP discovery URL（不接受 `https`/`ws`/`wss`、内嵌凭据或 query）。

回退规则：登录与签到只在**导航之前**失败（端点不可达、排队超时、target/WebSocket 建立失败、cookie 注入失败）时回退 HTTP 并 warn；导航之后失败直接报错，不会再发一次 HTTP 签到。回退不等于成功。浏览器支持多标签并发（默认 3），同一可注册域按公共后缀表串行。Site cookie 通过 host-only `Network.setCookies` 注入，读取仅限定当前 Site URL；会话期间及结束时**不发送任何 cookie 删除/清空命令**。第三方浏览器原有 cookie、本站点 cookie 和 SSO/跳转 host cookie 都留在原 jar，由浏览器/站点过期策略管理。若 endpoint 不支持 host-only 注入/当前 URL 回读，或接受公共后缀级 cookie，则不用于 Browser maintenance，导航前回退 HTTP。浏览器会话**不使用 Site 代理**，浏览器方式与 Site 代理互斥（两个方向都在保存时校验）。

**Fresh start（不做数据迁移）**：此功能版本要求使用空的数据目录。部署前请自行备份需要保留的文件，并手动删除/换新 `<data>`；应用不会自动删除用户数据。旧 schema/旧设置不兼容，启动时会明确提示重置数据目录，不迁移 `cdp.sync.enabled`、`obscura.enabled/url` 或旧 Site 记录。新 Site 默认 `maintenance_via = http`，CDP 开关默认关闭；需要浏览器登录/签到时由管理员手动开启开关并选择 Site 的浏览器方式。Profile 的 `render: true` 不再控制搜索，搜索始终走 HTTP。

**受管 Obscura 启动条件**：开关开启、未配置 `cdp.url`、至少有一个启用的 Site 选择浏览器登录/签到且没有自己的 `cdp_url`、二进制可用。不满足时设置页显示未启动原因（`no_managed_browser_sites`、`not_installed` 等）。受管 daemon 每次启动使用随机空闲端口并只绑定 `127.0.0.1`；只管理本进程拉起的 daemon，关闭开关或应用退出会停止它，外部服务不受影响。

**安装策略**（`obscura.install`，启动默认值 `CRAWLER_MEDIA_OBSCURA_INSTALL`）：

- `preinstalled`（默认）：不联网。**只需**把 v0.2.3 对应平台的原始发布归档（如 `obscura-x86_64-linux.tar.gz`）放为 `<data>/bin/obscura.tar.gz`；应用核对写死的 SHA-256 与大小后自行解出 `obscura`、`obscura-worker`。不接受只放二进制或手写哈希文件。
- `download`：首次需要时从 GitHub Releases 下载对应平台归档，按写死的 SHA-256 与大小校验，上限 200 MiB，通过后原子落盘。

信任根是代码中写死的哈希，不是发布仓库本身（详见 ADR-0007「信任模型」）。

**`network_mode: host` 风险**：受管 daemon 虽只绑定回环，但同宿主的任何进程都能访问该无鉴权 CDP 端口并读取站点 cookie；在共享宿主上请使用 bridge 网络或改用独立容器中的外部端点。

也可自行运行外部 Browser 并在设置中填 `cdp.url`。非法端点在保存时会被拒绝；保存合法地址只表示配置成功，不等于连通性验证。

## 下载器

qBittorrent / Transmission 通过 `/api/v1/downloaders` 配置（持久化到 SQLite）；进程也接受 `CRAWLER_MEDIA_QB_URL/USER/PASS` 环境变量覆盖。live 环境用 `docker-compose.yml` 起 qB（8080）/ TR（19091）容器。

下载器可停用（`enabled = false`）：停用后不再被自动投递选中，手动提交仍可用。

## 环境变量

主要服务启动配置由 [config](<../crates/api/src/config.rs>) 解析，并对未注册的 `CRAWLER_MEDIA_*` 打 warning。Web 构建、类型检查与测试要求 **Node.js ≥ 22**（`web/package.json`）；Docker 构建镜像使用 Node 22。部分运行时工具仍有独立读取点，下面是部署常用变量，不是全部运行时配置的唯一注册表；typed settings 的完整收敛仍待实施。

| 变量 | 默认 | 说明 |
|---|---|---|
| `CRAWLER_MEDIA_DATA` | `data` | 数据目录（SQLite / 缓存 / 图片 / 日志） |
| `CRAWLER_MEDIA_TOKEN` | 必填 | 实例访问令牌（用于 CLI 与受信服务调用） |
| `CRAWLER_MEDIA_ADMIN_PASSWORD` | 首次必填 | 管理员初始登录口令（不可与 TOKEN 相同，Argon2id 加盐哈希存储） |
| `CRAWLER_MEDIA_LISTEN` | `127.0.0.1:18765` | 监听地址 |
| `CRAWLER_MEDIA_UI` | 空 | 前端静态产物目录 |
| `CRAWLER_MEDIA_CORS_ORIGINS` | 空 | CORS 白名单（逗号分隔） |
| `CRAWLER_MEDIA_BROWSER` | 关 | 启动期 Browser 开关默认值（`1`/`true`/`yes`/`on`），仅在未保存过开关时生效。**只控制开关**：受管浏览器还需预置二进制或 `CRAWLER_MEDIA_OBSCURA_INSTALL=download`，且至少一个选择浏览器登录/签到的 Site，否则不会启动（设置页显示原因） |
| `CRAWLER_MEDIA_OBSCURA_INSTALL` | `preinstalled` | 受管 Obscura 安装策略启动默认值：`preinstalled`（不联网）或 `download`（按写死哈希校验后下载） |
| `CRAWLER_MEDIA_TMDB_KEY` | 空 | 非空 env／_FILE 固定 TMDB API Key 时，UI PUT 返回409；清除env pin后才可由UI修改 |
| `CRAWLER_MEDIA_TVDB_KEY` | 空 | TVDB API Key |
| `CRAWLER_MEDIA_QB_URL` / `_USER` / `_PASS` / `_CATEGORY` | 空 | qBittorrent 覆盖 |
| `CRAWLER_MEDIA_QB_PATH_MAP` | 空 | qB 路径映射 `容器路径=宿主路径,...` |
| `CRAWLER_MEDIA_TR_PATH_MAP` | 空 | Transmission 路径映射 |
| `CRAWLER_MEDIA_METADATA_PROXY` | 空 | 元数据代理 URL (如 `socks5://127.0.0.1:1080`) |
| `CRAWLER_MEDIA_METADATA_PROXY_USER` | 空 | 元数据代理认证用户名 |
| `CRAWLER_MEDIA_METADATA_PROXY_PASS` | 空 | 元数据代理认证密码 (支持 `_FILE`) |

### 密钥文件模式

保密变量都支持 `_FILE` 变体，值为存放密钥的文件路径；直接变量优先于文件。
用于 Docker Swarm / Kubernetes Secret 卷，避免密钥出现在 `docker inspect`
或 `/proc/<pid>/environ`：

```bash
CRAWLER_MEDIA_TOKEN_FILE=/run/secrets/cm_token
CRAWLER_MEDIA_QB_PASS_FILE=/run/secrets/qb_pass
CRAWLER_MEDIA_TMDB_KEY_FILE=/run/secrets/tmdb_key
CRAWLER_MEDIA_TVDB_KEY_FILE=/run/secrets/tvdb_key
CRAWLER_MEDIA_METADATA_PROXY_PASS_FILE=/run/secrets/proxy_pass
```

文件内容会被 trim；空文件视为未设置（不会用空值覆盖已有配置）。

## 凭证

Site 凭证（cookie / API key / RSS URL）按 [ADR-0003](<adr/0003-plaintext-site-credentials.md>) 明文持久化于 SQLite，须保护数据目录与一致备份。部署 secret 文件／env 必须不进入 Git 或 Docker 构建上下文；“不进版本库”不等于“不进数据库”。

## 数据库维护与升级注意

默认注册数据库轻量 optimize（每6小时）、Jobs历史清理（每天、90天、启用）、Playback历史清理（每天、90天、**关闭**）。首个启用任务可能立即执行，Jobs超90天终态历史可能在首次后台调度即删除；各def保留latest/tie组，运行／排队行不删。VACUUM默认关闭，绝不因每次DELETE自动重建库。

**当前控制缺陷必须了解：** 禁用running清理尚不能阻止下一批，修改更安全策略尚不刷新已queued的下一次快照；预览／报告存在计数截断与扫描预算缺口。不能把UI关闭当作已经停止，也不能把保存dry_run当作下一次一定只读。见 [总复核F01–F06](<audits/2026-10-13-database-api-review.md>) 与 [DB8合同](<superpowers/plans/2026-10-12-db8-maintenance-retention.md>)。

不要在运行时复制单个主体文件忽略WAL当备份；采用SQLite online backup或停机后对有关文件的一致备份。不要手工删除DB/WAL/SHM来缩空间。


### Obscura 受管浏览器补充（r9）
- **预置方式**（`OBSCURA_INSTALL=preinstalled`，默认值），以下两种任选其一：
  - 把官方发布包放到 `<data>/bin/obscura.tar.gz`，程序会按写死的 sha256 校验后解包；
  - 把 `obscura` 和 `obscura-worker` 两个二进制放进 `<data>/bin/`，并在同目录放一份 `obscura.sha256sums`（`sha256sum` 格式，每行一项）。两种方式下，每次启动都会重新校验二进制的 hash。注意：`obscura.sha256sums` 由部署者提供、与二进制放在同一目录，**只能防止文件损坏或误放，不能防篡改**——能改二进制的人也能改清单。需要防篡改时请使用官方发布包方式（hash 写死在程序里），或通过镜像/只读挂载保证 `<data>/bin` 可信。
- **局域网或内网站点**：Obscura 默认禁止访问私网地址。如果确实需要，设置 `CRAWLER_MEDIA_OBSCURA_ALLOW_PRIVATE_NETWORK=1`（默认关闭，注意 SSRF 风险）。
- **版本核对**用 `obscura --version`；不符时显示在状态的 `last_error` 里。
- **进程回收**：daemon 以独立进程组运行，停止时整组回收。应用崩溃后，由下次启动时的 pidfile 回收。
- **权限**：Browser 设置和 Obscura 状态接口仅管理员可读，因为里面有 CDP 端口和二进制路径。
