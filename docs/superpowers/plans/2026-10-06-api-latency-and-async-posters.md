# 接口低延迟与异步海报交付 Implementation Plan

> **历史状态（2026-10-13 整理）：** 订阅锁等待/异步海报的历史诊断与方案；当时测量和运行 commit 不代表当前部署或 SLA。当前 API 性能证据见下方 review，本文 Task 尚未逐项对账。
>
> 本文保留原设计、发现/任务 ID 与当时证据；日期本身不表示过时。旧执行/提交/清理命令仅作历史记录，不应直接重跑；复选框不是当前实现清单。当前依据：[数据库/API review](<../../audits/2026-10-13-database-api-review.md>) 与 [剩余计划](<2026-10-13-database-api-followup.md>)，未覆盖的旧 ID 仍须单独对账。

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking。
> 上述执行模式来自计划模板；仅使用当前环境实际提供的技能。未获用户明确授权不得创建 Agent Teams。本计划不授权实施、推送、重启现有服务或操作真实媒体数据。

**Goal:** 消除订阅读取造成的约 500 ms Store 锁重入等待及其对 jobs/auth 等接口的连带阻塞，在列表快速返回的同时保留浏览器异步加载海报的完整体验。

**Architecture:** 订阅 DTO 基于短锁内读取的本地快照生成，DTO 组装与图片路径检查在锁外执行，订阅列表不触发远程 Catalog。无 Library 文件的 Media 返回稳定的本地海报 URL；浏览器在独立图片请求中完成元数据解析、回源和缓存。图片获取具有单键去重、全局并发上限、超时和负缓存；网络、文件 IO 及同步 Catalog 运行于 blocking pool，绝不持有 Store/Jobs 锁等待。

**Tech Stack:** Rust workspace、Axum、Tokio、parking_lot、SQLite/rusqlite、现有 Catalog/PosterFetch traits、React/TypeScript、Node 原生测试、ego-browser。

## Global Constraints

- 计划日期：2026-10-06，时区 Asia/Shanghai。源码基线为本地 `main` 的 `37963e98a7508b905e793b4ccc7951ba582d4af4`；执行前重新记录实际 HEAD 和工作区状态。
- 本文是开发计划，不表示实现已完成。性能诊断时运行中的服务 `/api/v1/health` 返回 `commit=57f34936176c`；不能把 worktree 源码同步误认为服务已部署。
- 项目所有者要求重点优化 JSON 接口；图片自身可以异步慢慢加载，不能以永久空海报或删掉海报功能作为优化手段。
- 遵循 [AGENTS.md](<../../../AGENTS.md>) 与 [CONTEXT.md](<../../../CONTEXT.md>)，使用 Media、Subscribe、Library、Job 等领域词；不修改用户 [README.zh.md](<../../../README.zh.md>)。
- 每个 issue 是一个独立垂直 slice；执行前确定工单和依赖。已有未闭环设备/文件恢复问题不混入性能提交。
- `.rs` 文件 ≤800 行；生产与测试函数 ≤120 行；按责任拆分。改动过大的订阅/图片模块须拆成 sibling modules，不能靠删空行凑门禁。
- 不新增 domain IO，不让其他 crate 依赖 api。尽量复用已有依赖与 trait，不新增通用任务系统、全局响应缓存或数据库连接池作为本轮前置条件。
- 原有 public 静态图片路由保持 public，新 Media 海报入口同样 public。不得依赖 cookie 或原生 `<img>` 无法携带的 Bearer Header。
- public 图片入口只提供图片，不提供 Media/Subscribe DTO；只接受 Media UUID，不接受任意上游 URL，不用新增图片路由绕过现有 Library 图片可见性约束。
- Store/Jobs 的同步锁不得跨 `.await`，不得覆盖远程 Catalog、HTTP、磁盘图片读写、队列等待或睡眠；同步 IO 与 Catalog 放入 `spawn_blocking`。单纯把等待放进 blocking pool但仍持 Store 锁，不算修复。
- 意外错误使用结构化 error，可恢复降级使用 warn；记录操作、对象、耗时与错误。新性能/图片日志不得包含 token、API key 或携带凭据的完整上游 URL。既有播放日志风险例外不扩展到新增日志。
- 每个触及 crate 单独运行 `cargo test -p <crate> --offline`；最后 `cargo test --workspace --offline`。新增测试不得访问真实 TMDB、图床、Downloader 或启动 Chromium。
- 请求超时必须包含排队、元数据获取及图片读取的整体耗时；仅 timeout `JoinHandle` 不会取消阻塞 HTTP，底层 HTTP 自身也必须有期限。

## 1. 已测事实、原因与目标

### 1.1 实测基线（不是性能 SLA 的长期统计）

测试地址：前端 `http://127.0.0.1:3334`，API `http://127.0.0.1:18765`；使用用户已登录的 ego-browser session，真实 GET 请求，不执行 Job、不更改业务状态。

| 接口（省略 `/api/v1`） | 独立请求 | 与 `/subscriptions` 并发 | 页面实测峰值 |
|---|---:|---:|---:|
| `/subscriptions` | 508–514 ms | 509–514 ms | 1,036 ms |
| `/jobs?limit=50` | 19–30 ms | 542–557 ms | 本次峰值约 557 ms |
| `/rule-sets` | 3–9 ms | 514–542 ms | 1,037 ms |
| `/auth/me` | 2–8 ms | 513–554 ms | 同上并发实验 |
| `/subscriptions/automation-readiness` | 4–5 ms | 524–556 ms | 1,038 ms |
| `/subscriptions/today-arrivals` | 3–5 ms | 518–542 ms | 529 ms |
| `/catalog/cache?view=aggregate` | 303–312 ms | 未测对照 | 独立待分析 |
| `/ledger` | 14–15 ms | 未测对照 | — |
| `/playback/activity?scope=visible` | 2–3 ms | 未测对照 | — |

去掉 subscriptions 的三轮并发对照，jobs/rule-sets/auth 恢复至约 3–24 ms；加上 subscriptions 后三轮均超过 500 ms。开发前端观察到重复请求；不能仅凭此认定线上也有同样的重复调用。

### 1.2 已确认源码调用链

1. [list_subscriptions](<../../../crates/api/src/http/subscriptions/views.rs#L290-L329>)持 `state.store.lock()` 构建 DTO。
2. [poster_url_for](<../../../crates/api/src/http/subscriptions/views.rs#L29-L64>)在无本地海报时调用 `Catalog::poster_url`。
3. [TmdbCatalog::poster_url](<../../../crates/api/src/catalog.rs#L312-L316>) → `Tmdb::details_poster` → cache miss 时 `TmdbHttp::get`。
4. [TmdbHttp::api_key](<../../../crates/api/src/tmdb_http.rs#L22-L30>)再次 `try_lock_for(500 ms)` 读取同一个 Store。非重入锁导致等待超时，并被误报告成 key 未配置。
5. [jobs](<../../../crates/api/src/http/jobs.rs#L153-L196>)及 API 鉴权也访问 Store，因此受到连带阻塞。

Create/Patch/Get Subscribe 响应也复用 `subscription_json`，必须一并迁移，否则列表修好后详情/更新仍可触发同一问题。

### 1.3 本轮验收指标

- 离线合同：订阅列表/创建后 DTO/更新后 DTO生成 **零远程 Catalog 或图片调用**；慢图片请求进行中，jobs/auth/list 仍能完成。
- 本地浏览器验收：同一运行环境、同一数据，预热后至少 30 轮；subscriptions/jobs/auth/rule-sets P95 ≤100 ms、单次 >250 ms 记录原因；此前稳定约 500 ms 等待消失。此阈值是本项目本地验收目标，不是所有硬件/公网下的承诺。
- 图片：无缓存仍返回可用的本地 URL；点击/滚动卡片后浏览器首次 `<img>` 请求最终收到图片；失败有占位且不会永久缓存，冷图请求不拖慢 JSON 接口。
- 以确定性的 fake barriers/超时验证隔离合同，不用 CI 上墙钟微小差异证明性能。运行基准应单列样本、版本、数据量、冷/热状态。

## 2. 写入范围与边界

以下新增接口均为拟议接口，不是现有 API；执行任务先核对类型与依赖版本。

| 任务 | 文件/模块 | 职责 |
|---|---|---|
| T1 | 新增 `crates/api/tests/subscription_latency.rs` | 通过真实 router + fake Catalog/图片依赖证明 JSON 与上游隔离 |
| T2 | 新增 `crates/api/src/http/media_posters.rs`；新增 `crates/api/src/media_posters/{mod.rs,service.rs,cache.rs,http.rs}` | public UUID 图片入口、协调获取、TTL/原子缓存、受限 HTTP |
| T2 | 修改 `crates/api/src/lib.rs`、`crates/api/src/http/mod.rs`、`crates/api/src/management/state.rs` | 模块注册、public 路由、共享 service 实例及测试注入 |
| T2 | 新增 `crates/api/tests/media_posters.rs` | 图片 bytes/MIME、并发去重、失败重试、匿名读取/输入约束 |
| T3 | 新增 `crates/api/src/http/subscriptions/{snapshot.rs,poster_view.rs}`；修改 `views.rs` | 本地快照读取、短锁、纯 DTO、海报相对 URL |
| T3 | 修改 `crates/api/src/http/subscriptions/{create.rs,patch.rs}`、`crates/api/src/http/subscriptions.rs` | 全部 DTO调用点迁移，详情额外 Catalog操作阻塞隔离 |
| T4 | 修改 `web/lib/image-proxy.ts`；新增 `web/lib/image-proxy.test.ts` | 验证本地相对海报 URL直连，不被错误包进远程代理 |
| T4 | 修改 `web/components/poster-image.tsx`（仅有合同不满足时） | 原生图片加载及失败占位，不添加阻塞列表的图片预取 |
| T5 | 新增 `scripts/measure-api-latency.mjs`；新增本轮测量报告 | GET-only可重复采样、部署/浏览器验收证据 |

`/catalog/cache?view=aggregate` 优化、jobs自身查询规模优化、订阅详情全部远程元数据异步化、全局 SQLite 架构和前端 StrictMode改造均为独立后续 slice。可以测量并报告，不在未复现具体瓶颈前顺手重写。

## 3. 海报与快照合同

### 3.1 稳定图片 URL 与优先级

新增 public `GET /api/v1/media/{media_id}/poster`，输出真实图片 Content-Type。只接受标准 Media UUID，服务器从已有 Media 解析 TMDB alias；不把 `url=` 参数作为上游输入。

订阅 DTO的 `media.poster_url` 保持 `string | null`：

```rust
// 新增于 http/subscriptions/poster_view.rs；纯函数，无 IO。
pub(crate) enum SubscriptionPoster {
    Library(domain::LedgerId),
    Catalog(domain::MediaId),
    None,
}

pub(crate) fn poster_url(poster: SubscriptionPoster) -> Option<String> {
    match poster {
        SubscriptionPoster::Library(id) =>
            Some(crate::http::library::artwork_url("posters", id)),
        SubscriptionPoster::Catalog(id) =>
            Some(format!("/media/{id}/poster")),
        SubscriptionPoster::None => None,
    }
}
```

- 有已有可用 Library poster：继续用现有 `/posters/{ledger_id}`，尊重手动选择，不改现有路由或图片文件。
- 无 Library poster、有 TMDB alias：无论 metadata cache命中与否，立即返回 `/media/{uuid}/poster`；图片请求自行解析 URL及回源。
- 没有支持的 alias且无本地图：null，保留现有占位；不使用错误 alias、不凭 Media title猜图。
- 新 Media 路由只返回 Catalog海报，不查找/暴露受限 Library私有图片，不把下载图片写入影片目录。
- 本地路径检查在快照后锁外进行；一次列表只读取一次 ledger，分组到 Media，而不是每个 Subscribe全表扫描。

### 3.2 新图片 service 接口与限制

```rust
// media_posters/mod.rs；生产/测试共享的小接口。
#[derive(Clone)]
pub struct PosterBytes {
    pub content_type: String,
    pub bytes: std::sync::Arc<[u8]>,
}

#[derive(Clone, Debug, thiserror::Error)]
pub enum PosterError {
    #[error("media or poster not found")]
    NotFound,
    #[error("poster request timed out")]
    Timeout,
    #[error("invalid upstream image")]
    InvalidImage,
    #[error("poster upstream failed: {0}")]
    Upstream(String),
    #[error("poster IO failed: {0}")]
    Io(String),
}

#[async_trait::async_trait]
pub trait MediaPosterSource: Send + Sync {
    async fn get(&self, media_id: domain::MediaId) -> Result<PosterBytes, PosterError>;
}
```

- `ApiState`新增 `media_posters: Arc<dyn MediaPosterSource>`；默认生产实现持 `Arc<Mutex<Store>>`、`Arc<dyn Catalog>` 和受限 HTTP依赖。测试提供 fake；`.with_catalog(...)`必须确保 source拿到更新后的 Catalog，而不是初始化时的 EmptyCatalog。用明确 builder或重新构建默认 source，禁止测试/生产依赖错配。
- `HttpPoster`现有接口仅返回 bytes且无本服务整体期限，不直接假定它具备 MIME、大小及超时合同。新增 `media_posters/http.rs` 受限获取器，复用图片代理的主机校验/重定向规则，不降低已有 SSRF保护。
- 元数据/图片网络、响应体读取及同步文件操作在 blocking线程执行，底层元数据与图片阶段各≤4秒；排队/全流程≤10秒。现有 `Catalog`没有逐请求 deadline，生产需新增受限 Catalog视图构造或 HTTP timeout参数，不能只给外层 Future加timeout后放任blocking任务无限运行。
- 同一 Media单键协调：Tokio mutex gate；取得 gate后再次查缓存。全局 semaphore=4，在进入 blocking pool前获取 permit；不要在blocking线程里等待 gate/permit。
- 内存正缓存 TTL=24小时；磁盘缓存位于 data/cache/media-posters，命名为 Media UUID，记录 MIME、长度、成功时间，最大图片8 MiB。校验 Content-Type与实际图像格式（JPEG/PNG/WebP）并限制读入大小，拒绝 HTML/空文件。
- 同目录 `create_new` 唯一临时文件，写入并同步后原子替换完整缓存文件；失败不留下可读的部分缓存。新缓存不修改现有 poster.jpg、不影响手动选择。
- 负缓存仅 NotFound/上游失败/timeout，TTL=30秒；不要负缓存 Store查询错误或磁盘损坏以掩盖系统故障。TTL结束新请求重试。
- 成功：200 + Content-Type + `Cache-Control: public, max-age=86400`；NotFound：404 + `Cache-Control: no-store`；Upstream/InvalidImage：502；Timeout：504；IO/Store故障：500。非200响应不携带长效 public cache header。
- 无 token/native image合同：不要求 Bearer；UUID错误400、不存在404。并发去重与上游并发上限应阻止恶意多请求造成阻塞池暴涨。

## 4. 实施任务

### Task T1：建立红灯回归与准确的测量基线

**Files:** Create `crates/api/tests/subscription_latency.rs`；复用已有 `crates/api/tests/management/common.rs` 的请求、fake Fetcher及 state构造模式，测试 fixture放新文件的 sibling support模块，避免超长单测。

**Interfaces:** 消费现有 `ApiState::with_catalog`、`Catalog`、`api::router`、`tower::ServiceExt`；本任务测试支撑新增 `fixture(root, catalog)`，返回带1个无 ledger/无缓存海报的 TMDB Subscribe的 Router、MediaId。Fixture初始化用现有 Store写入 API，并沿用已有 Subscribe构造字段，不以真实网络建立测试数据。

- [ ] 建立 `ForbiddenPosterCatalog`：其他 Catalog必需方法返回现有空 fixture结果，唯一 poster方法返回错误；只有测试“非法出站请求”的 seam允许计数。

```rust
fn poster_url(&self, _: domain::MediaKind, _: &str) -> Result<Option<String>, String> {
    self.poster_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    Err("list must not request remote posters".into())
}
```

- [ ] 在 router seam写列表测试：GET `/api/v1/subscriptions` 携合法测试 token，200，海报字段应为 `/media/{media_id}/poster`，`poster_calls == 0`。冷缓存且有 TMDB alias必测；单独跑 `cargo test -p api --offline --test subscription_latency`，旧实现应因调用Catalog/无稳定URL失败，不把编译失败当红灯证据。
- [ ] 同一文件增加 GET detail、POST create、PATCH响应合同，至少断言海报逻辑不触发 `Catalog::poster_url`；详情现有 season_episodes是另一合同，不在此计数。
- [ ] 建立未来 T2用到的可控 barrier fake图片 source：收到 get时 `started.notify_one()`，随后 `release.notified().await`，最后返回 PNG fixture。测试支撑会在 T2接口落地时接入，不引入真实 sleep。
- [ ] 保存实测版本、样本和命令；独立提交红灯测试（按仓库工单流程），此时不声称本任务修复通过。

### Task T2：增加独立 public Media海报请求及受限缓存服务

**Files:** Create T2地图中的模块与 `crates/api/tests/media_posters.rs`；Modify模块树/public router/ApiState builders。图片服务只有一个应用级实例，不每次请求new缓存。

**Interfaces:** 生产 `MediaPosterSource` 实现消费 Store Media快照与 Catalog；Http返回`PosterBytes`。路由使用 `State<ApiState>` + `Path<String>` 并调用 `state.media_posters.get(id).await`。

- [ ] 先写假source路由测试，fixture PNG使用仓库已有golden或固定合法小PNG文件。断言无Authorization仍能读取图片；UUID不存在/非法分别404/400；恶意`url`输入不能改变目标。

```rust
let response = app.oneshot(
    axum::http::Request::get(format!("/api/v1/media/{media_id}/poster"))
        .body(axum::body::Body::empty()).unwrap()
).await.unwrap();
assert_eq!(response.status(), axum::http::StatusCode::OK);
assert_eq!(response.headers()["content-type"], "image/png");
assert_eq!(axum::body::to_bytes(response.into_body(), 1024).await.unwrap(), png_bytes);
```

- [ ] 运行 `cargo test -p api --offline --test media_posters`确认路由红灯，再注册public路由及source。
- [ ] 实现冷请求快照边界：blocking中短锁取 Media/alias，退出锁；之后才调用Catalog以及图床。图片来源只用支持的alias；不输出密钥。

```rust
let media = {
    let store = store.lock();
    store.get_media(media_id).map_err(|e| PosterError::Io(e.to_string()))?
}; // guard在此释放
let media = media.ok_or(PosterError::NotFound)?;
// 后续Catalog/HTTP在blocking上下文，且不持Store guard。
```

- [ ] 加入gate、semaphore、deadline、8MiB限制、MIME验证、缓存原子写入及错误HTTP映射。同步Catalog获取本轮显式的底层超时配置；测试时fake无需真实网络。
- [ ] 用fake时钟测试正缓存24h、负缓存30s的命中/过期；并发20个同UUID请求一次成功回源（请求去重本身是出站合同，计数允许）。不同UUID最多4个同时回源；高并发队列按整体期限退出。
- [ ] 强制图片IO失败/非法HTML/timeout/JoinError，验证非200不长期缓存、TTL结束能够恢复，临时文件不被当作成功缓存。
- [ ] 运行单独API离线测试，review public边界、不出现私有Library图片旁路，提交 `feat(api): serve media posters independently of subscription JSON`。

### Task T3：迁移所有 Subscribe DTO到短锁本地快照

**Files:** Create `snapshot.rs`、`poster_view.rs`；Modify views、create、patch及subscriptions模块。更新所有`subscription_json`调用点及 re-export，不保留仍可持锁调用Catalog的旧wrapper。

**Interfaces:** 新增 `SubscriptionViewSnapshot`携带已有Media/Subscribe、facts推导的进度、pending计数、时间、Library海报候选（owned路径/ledger id）以及DTO全部现有字段。`load_subscription_view(store: &Store, subscribe: &Subscribe, media: &Media) -> Result<SubscriptionViewSnapshot, StoreError>`仅本地DB；`render_subscription(snapshot: SubscriptionViewSnapshot) -> Value`不接受Store/Catalog引用。`progress`移除未使用的Catalog参数并更新调用者。

- [ ] 对照现有 `subscription_json`列出完整JSON字段，golden比较保持时间、progress、download routing、wash_cut、保留策略等字段不变，仅无本地海报时poster_url改为本地Media路由。
- [ ] 将T1列表失败测试跑到红；新增有本地用户海报优先测试、多个Subscribe同Media一致URL测试及空alias占位测试。
- [ ] 锁内只读取role、Subscribe列表、Media/facts/pending/time及必要cache快照；一次批量ledger加载建立Media映射，已选图片路径的exists检查移到blocking锁外。

```rust
let state_for_read = state.clone();
let snapshots = tokio::task::spawn_blocking(move || {
    let store = state_for_read.store.lock();
    load_visible_subscription_views(&store, user_id)
}).await; // join错误与Store错误分别显式日志+500
// 此处无Store锁。路径检查/DTO render在blocking pool完成，最后Json返回。
```

`load_visible_subscription_views`在本任务定义，返回 `Result<Vec<SubscriptionViewSnapshot>, StoreError>`；沿用admin全局/member自己订阅规则。若本地DB阶段过慢，测量并优化批量查询，不将remote加入snapshot。

- [ ] Create/Patch/Get响应改用同一快照与render入口。Get当前已有`Catalog::season_episodes`放入锁外 `spawn_blocking`，不得回归已修过的wanted_json锁问题；该详情调用本轮可等待元数据，但不能阻塞jobs/auth。未做到详情完全异步元数据时在验收报告注明，不称所有JSON都零上游。
- [ ] 接入T1 barrier fake：起一个冷 `/media/{id}/poster` 请求并等待fake通知开始；在release前要求以下三个router请求完成，最后release图片请求并验证200。这是隔离合同测试，1秒测试超时是死锁探测上限，不是100ms性能结论。

```rust
let concurrent = tokio::time::timeout(std::time::Duration::from_secs(1), async {
    tokio::join!(
        app.clone().oneshot(authed_get("/api/v1/subscriptions")),
        app.clone().oneshot(authed_get("/api/v1/jobs?limit=50")),
        app.clone().oneshot(authed_get("/api/v1/auth/me")),
    )
}).await;
release.notify_one(); // 先释放，再assert，失败也不留下悬挂fake
assert!(concurrent.is_ok(), "slow image must not hold Store or block JSON");
```

`authed_get(path)`在T1 support中用固定fixture token构造空body GET，禁止live token。

- [ ] 用共享 Store的真实 `TmdbHttp` + 空目录缓存补端到端依赖测试：JSON列表不调用TMDB因此不触发500ms重入；图片请求可以读API key（lock已释放）。禁止仅用不会重入的EmptyCatalog冒充此链路测试。
- [ ] 跑 `cargo test -p api --offline`以及既有 management subscription/playback/contracts测试，提交 `fix(api): release store before rendering subscription views`。

### Task T4：验证图片异步体验，避免前端退化

**Files:** Modify `web/lib/image-proxy.ts`（仅必要时）、`web/components/poster-image.tsx`（仅必要时）；Create `web/lib/image-proxy.test.ts`。现有imageUrl已支持相对URL，优先保持实现不变，测试锁住新合同。

**Interfaces:** 消费T3 `media.poster_url=/media/{uuid}/poster`；现有`imageUrl`最终解析到API base，不能把本地URL编码成 `images/proxy?url=http://127...`，也不能加Authorization到图片URL。

- [ ] 按现有 Node测试的相对import/alias约定补URL行为测试。如果image-proxy依赖alias阻止Node直接运行，提取一个仅字符串解析的pure helper到 `web/lib/image-url-shape.ts`，主模块与测试都调用它，不在测试重写逻辑。

```typescript
assert.equal(classifyImageUrl("/media/11111111-1111-1111-1111-111111111111/poster"), "local");
assert.equal(classifyImageUrl("https://image.tmdb.org/t/p/w342/a.jpg"), "remote");
```

`classifyImageUrl(url: string): "local" | "remote"`仅当需解除alias测试问题时新增，返回值由 `/^https?:\/\//i`判断，`imageUrl`消费它决定现有分支；不顺手更改所有图片路由。

- [ ] 检查PosterImage收到非空本地URL时会发 `<img>`，加载中显示现有占位，成功后显示图，404/502走现有错误fallback；不在列表load前 `await image`。保持lazy加载、比例与已有用户封面优先级。
- [ ] 用浏览器fake慢图或本轮fake服务：列表文字/按钮先可用，图片随后显示；首轮失败后服务恢复并重新请求时成功，不因负缓存永远占位。
- [ ] `cd web` 后运行 `node --test`、`./node_modules/.bin/tsc --noEmit`；仅实际源代码变化时提交前端独立slice。

### Task T5：可重复测量、部署确认与发布验收

**Files:** Create `scripts/measure-api-latency.mjs`；Create `docs/superpowers/plans/2026-10-06-api-latency-verification.md`（执行阶段生成真实结果，不提前填写通过）；必要更新API合同记录新增public海报路由。

**Interfaces:** 测量脚本仅GET，参数环境变量 `API_BASE`、`API_TOKEN`、`SAMPLES`（默认30）；禁止打印token。输出路径/status/totalMs/TTFB/读取耗时、min/median/P95/max；不执行Job tick、缓存删除或业务写入。账号通过用户授权读取，不能把真实token提交。

- [ ] 编写测量核心，先读取 health打印commit再采样；30轮warm顺序与有/无subscriptions并发分别输出。请求8秒AbortSignal，非200标记失败，禁止将401快速返回算优化成功。

```javascript
const base = process.env.API_BASE;
const token = process.env.API_TOKEN;
if (!base || !token) throw new Error("API_BASE and API_TOKEN required");
async function measure(path) {
  const start = performance.now();
  const response = await fetch(`${base}${path}`, {
    headers: { Authorization: `Bearer ${token}` },
    signal: AbortSignal.timeout(8000),
  });
  const headersAt = performance.now();
  await response.arrayBuffer();
  return { path, status: response.status,
    ttfbMs: headersAt-start, totalMs: performance.now()-start };
}
const batch = await Promise.all([
  "/subscriptions", "/rule-sets", "/jobs?limit=50", "/auth/me"
].map(measure));
console.log(JSON.stringify(batch));
```

实现循环与统计排序 `p95 = sorted[Math.ceil(n*0.95)-1]`，同时支持无订阅对照；令目标关键GET的P95>100ms或非200使脚本非零退出，输出raw samples便于复核。

- [ ] 在旧服务至少运行一次建立red-capable证据。旧订阅≈510ms时应失败。若生产无法稳定复现，使用T3 deterministic fake隔离测试作为主门禁，不编造性能结果。
- [ ] 执行前 `git worktree list --porcelain` 和两边status，选择已确认worktree并同步main；保留用户ADR等未提交修改。分支有分叉时禁止硬重置，先确认合并策略。
- [ ] 构建将运行的worktree二进制与Web artifacts。经用户授权停止/重启既有服务，保持原地址/数据目录/配置，验证 `/health.commit`等于目标SHA；不启动另一个空数据服务冒充优化效果，不仅以源码mtime判部署成功。
- [ ] ego-browser在同一个TaskSpace复测真实页面；采集至少30轮样本，不清用户cookie/cache、不触发手动Job。warm/cold图片测量使用测试Media和专用图片缓存fixture，禁止删除真实图库cache。
- [ ] JSON验收达到目标，截图/网络记录证明列表先可用、图像随后加载；若未达到，分别测Store等待、DB读取、render与图像出站，限定同一变量验证。
- [ ] `/catalog/cache?view=aggregate`记录独立3xxms样本、cache行数及响应字节数。当前 [list_catalog_cache](<../../../crates/api/src/catalog_cache.rs#L34-L97>)已在读取rows后释放Store再聚合，不把它认定为同一500ms锁重入；另开issue检查JSON解析、无分页全量返回及CPU耗时，不在本轮未测量前套用解法。
- [ ] 检查jobs既有逐def查询与前端轮询：只在同环境大数据或重复请求有证据时开后续优化，保留活动/历史查询完整性及SSE契约；不移除StrictMode来隐藏后端问题。
- [ ] 运行每个触及crate测试、全workspace离线测试、Web tests/tsc、`git diff --check`。按slice提交，发布实际版本/样本/未执行环境/剩余限制。

## 5. 执行顺序与回滚

- 顺序：T1红灯 → T2图片入口 → T3 DTO短锁 → T4体验合同 → T5实测。T2路由与服务应先落地，T3才输出新URL，避免中间提交把卡片指向404。
- T2/T3都涉及ApiState或http module注册，不并发编辑同文件；多人执行按write scopes和依赖划分，默认单线实施。
- 每个slice完成独立review及测试。海报服务不是通用调度器，本轮不增加复杂跨进程任务缓存；若多进程部署需另列限制。
- 回滚应成套回滚T3 URL输出与T2入口，不保留坏URL；不要删除旧Library海报或Catalog缓存。新增缓存是可再生数据，代码回滚不删除用户媒体文件。

## 6. 最终验收清单

- [ ] 原subscriptions约500ms重入锁等待消失，jobs/auth在慢图片和并发订阅请求下不受上游牵连。
- [ ] Cache miss列表仍返回可加载海报URL；浏览器无需cookie/Bearer可加载public图片，失败占位和重试正确。
- [ ] 无远程操作/磁盘图片IO在Store/Jobs锁内；同步Catalog及IO不阻塞Tokio执行线程。
- [ ] 同键去重、全局4并发、整体10秒期限、负缓存30秒和正缓存24h已在fake测试验证；底层阻塞请求也有期限。
- [ ] 手动海报、现有Library图片可见性、Subscribe隔离/进度/保留策略/DTO字段无回归。
- [ ] 新增GET路由、Create/Patch/Get DTO调用点全部更新，包含public注册与Frontend URL解析。
- [ ] 本地30轮实测报告P95与版本；缺失浏览器/环境验证明确写未执行；cache aggregate独立问题另列。
- [ ] crate/workspace/Web/tsc/diff门禁通过；用户README和原worktree用户改动未被覆盖；无凭据入文档/脚本/日志。

## 7. 计划自检结果

- **需求覆盖：** JSON快速返回及锁隔离由T1/T3/T5负责；异步图片最终显示由T2/T4负责；jobs并发回归由T3/T5负责；运行版本/worktree对应由T5负责。
- **非目标明确：** 本计划不承诺修复既有设备绑定、文件恢复问题；cache聚合300ms与jobs自身规模问题作为独立性能slice，不虚构完成。
- **类型一致：** MediaPosterSource/PosterBytes/PosterError由T2定义；snapshot与render由T3定义；T1先建立红灯断言，T2注入fake；T4仍使用既有poster_url字段。
- **安全与体验：** public图片不改业务鉴权；没有用永久null降低功能；没有把进程同步或源码合并当作部署完成。
