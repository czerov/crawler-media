# Library 删除与 NFO 重新扫描 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. 本仓库执行时使用当前可用的 `executing-plans`；不要调用不存在的技能。只有用户选择子代理执行时才派发普通 subagent，不创建 Agent Teams。

**Goal:** 允许删除含文件记录的 Library，停止关联任务并清除其扫描/探测数据，不改动磁盘文件和用户观看数据；重新添加同一目录时优先读取 NFO，复用可靠匹配到的 MediaId，恢复观看进度。

**Architecture:** 删除使用持久化生命周期与可恢复清理流程：先冻结库并阻止旧任务发布，再清除独占台账和派生数据，最后删除根目录配置。Media 身份与 Playback 数据独立保留；NFO 提供作品级外部别名，以类型和外部 ID 找回 Media，而非用文件路径猜测身份。四个 SQLite 文件不假定存在跨连接原子事务，以删除日志和幂等步骤保证失败、重启后可继续。

**Tech Stack:** Rust workspace、SQLite/rusqlite、Tokio、parking_lot、roxmltree、React/TypeScript；现有 Catalog、PosterFetch、FingerprintEngine/捕获引擎的 fixture seam。

## Global Constraints

- 实施范围仅为 Library 删除、任务隔离、NFO 优先识别与 Playback 身份恢复；不实现路径缓存迁移、未归属台账认领或观看历史清空选项。
- 删除 Library 不删除/移动/改写视频、STRM、字幕、NFO、海报、fanart、分集剧照；取消后的旧任务也不得继续写这些侧车文件。
- 清除归属该库的 ledger、file_meta、fingerprint_cache、fingerprint samples/attempts、probe_stage_state；Media、Playback 进度/已看/收藏、Subscribe、Downloader 和 Catalog 缓存保留。
- 删除归属使用删除前的严格唯一所有权快照，最长根目录获胜；不使用 `LIKE root || '%'`，不把默认库 fallback 当成删除所有权。
- 嵌套子库、另一库的同 Media 文件不能被清理。歧义归属在删除预检返回冲突，不猜测。
- 同类型最后一个库暂时继续保护：这是现有默认 Library/Transfer 契约，放开到零个库是另一个需求，不在本计划中悄悄改变。
- 新 Library 的 `detect_intros=false`、`enable_fingerprint=false`；不因旧缓存、NFO 或旧任务而开启。关闭标记检测不阻止媒体信息探测。
- Media 身份为内部 MediaId；外部别名须包括媒体类型，TMDB movie/tv 数字 ID 不能跨类型合并。
- NFO 的元数据不等于媒体流探测缓存；重新扫描后仍安排 ffprobe 获得编码、音轨、字幕、可靠时长。不声称 NFO 可替代实际文件校验。
- 测试使用临时目录、fake Catalog/捕获引擎，不请求真实 TMDB/IntroDB，不拉真实远程 STRM。
- `cargo test -p <crate>` 对每个触及的 crate 单独运行，最后一次 `cargo test --workspace`；绿色后当前分支提交，只提交本请求相关改动。
- Rust 文件硬上限 800 行、生产/测试函数硬上限 120 行；NFO 主文件当前 792 行，不向它堆入身份解析逻辑，新增 sibling module。
- 图像读取路由仍 public，视频流仍按 Playback 协议认证；取消/隐藏库不能靠把全部图像端点改为登录必需来实现。
- 命名遵守 CONTEXT.md：Media、Library、Subscribe、Transfer、Scrape、NFO、Playback。日志使用结构化 library_id、ledger_id、job_id、operation_id 和错误上下文。

---

## 已核验的基线与设计决定

基线 HEAD：`1fef14a`（实现前再次确认 HEAD 和用户并行改动）。

1. `crates/store/src/libraries.rs::delete_library` 保护最后一个同类型库，按路径字符串前缀查 ledger，有数据便拒绝；真正删除只涉及 app.db 配置。
2. `library_for_path` 有默认库 fallback；`library_for_path_strict` 不 fallback。禁止删根目录后再计算待清理范围。
3. `crates/store/src/library.rs::delete_file_meta_for_ledger` 已清 file_meta、指纹、阶段和样本；复用这个数据库清理基础，不调用会删物理文件的条目删除 handler。
4. `app.db` 存 Library/Media，`library.db` 存 ledger/探测，`subscribe.db` 存 Playback/Subscribe。Store::open_files 使用四条独立 WAL 连接，不存在一条普通 Transaction 覆盖四个文件。
5. 进度键为 UserId + MediaId + season + episode。保留 progress 而删除 Media，或者重新建立不同 MediaId，都不能保证恢复。
6. 扫描 record_paths 先按 title/kind/year 匹配；incoming Media 没有外部 ID。通用 ensure_media 支持外部 ID，但目前扫描没有完整利用 NFO 身份。
7. NfoMeta 尚无作品级外部别名字段；TransferredFile 只带 Release。episode NFO 中 title/uniqueid 可能是单集身份，不能直接成为 TV Media。
8. 扫描是 spawn_blocking；取消 JoinHandle 不会停止正在执行的阻塞闭包。现有 ffprobe 使用 Command::output，必须提供真实子进程取消，不能以 abort 代替。
9. Subscribe 已显式指定的 library_id 查不到会报缺少目标库；保留该指向，不置空、不静默退默认库。增加可读状态并在最终 Transfer 前重检。

### 用户可见语义

- 删除含台账的库允许执行；确认文案明确文件不动、探测缓存清除、进度保留。
- 大库/有活跃任务时 `DELETE` 返回 202 和 operation_id。无论是否很快清理完成，都使用同一操作协议，避免前端猜结果。
- 删除中库不可启动扫描、Scrape、探测、Transfer 或新 Playback；列表保留一张“删除中”管理卡，普通浏览不可见。
- 删除成功意味着旧任务已经不能再产生该库的写入，不意味着“已发取消通知”。无法停止工作时操作失败/可重试，库保持冻结，不假报成功。
- 同一路径重建生成新 ledger id；旧 MediaId 在可靠身份一致时复用。没有可靠身份不强行恢复进度，保留旧观看数据等待后续可靠识别。
- 既有自动识别入口若后来匹配到了旧 Media 的外部 ID，必须在写入前复用旧身份，而非给新 Media 简单贴上相同别名。

## 文件结构与边界

| 文件 | 职责 |
|---|---|
| 新 `crates/store/src/library_deletion.rs` | 持久删除操作、范围快照、library.db 幂等清理与 app.db 配置收尾 |
| 改 `crates/store/src/{lib.rs,libraries.rs,schema.rs}`；新 `schema/library_deletion.rs` | 导出、生命周期过滤、迁移；旧 delete_library 转向安全协议，禁止绕过 |
| 新 `crates/library/src/work_control.rs` | 可取消外部进程与协作检查，独立于 api |
| 改 `crates/library/src/{lib.rs,tracks/probe.rs}` 与 marker 捕获模块 | ffprobe/ffmpeg 取消支持，兼容原调用者的默认不取消接口 |
| 新 `crates/api/src/library_lifecycle.rs` | 管理每库工作租约、删除用例、取消与等待、重启恢复 |
| 改 `management/state.rs`、扫描、探测、声纹、Scrape/Transfer 入口 | 领取生命周期租约、发布前校验与取消传递 |
| 新 `crates/library/src/nfo_identity.rs` | 解析作品级别名；movie/tvshow/episodedetails 分清楚 |
| 新 `crates/library/src/watch_identity.rs` | 有边界地查找 movie NFO、tvshow NFO 与分集字段 |
| 改 `watch.rs`、`watch_ledger.rs`、`store/media.rs`、`auto_resolve.rs` | 身份传递、可靠找回 MediaId、避免联网重识别 |
| 改 `http/library_config.rs`；新 `http/library_deletion.rs` | 删除 HTTP 操作与状态路由 |
| 改 `web/lib/api/libraries.ts`、两个 Library 管理/详情视图 | 删除操作状态与一致确认文案 |
| 新 `docs/adr/0012-library-removal.md` | 明确库、文件事实、Media/观看身份不同生命周期 |

模块注册跟随现有 `lib.rs`/`http/mod.rs`；不要为了计划大范围重构不相关代码。

## Task 1: 持久化删除范围与恢复协议

**Files:**
- Create: `crates/store/src/library_deletion.rs`, `crates/store/src/schema/library_deletion.rs`
- Modify: `crates/store/src/lib.rs`, `crates/store/src/schema.rs`, `crates/store/src/libraries.rs`
- Test: `crates/store/tests/library_removal.rs`

**Interfaces:**
- Produces（Store 新公开接口，后续任务必须使用相同名字）:
```rust
pub struct LibraryRemoval {
    pub operation_id: String,
    pub library_id: String,
    pub state: String, // frozen | draining | cleaning | succeeded | failed
    pub removed_ledger_count: usize,
    pub error: Option<String>,
}
pub fn begin_library_removal(&self, library_id: &str)
    -> Result<LibraryRemoval, StoreError>;
pub fn library_removal(&self, operation_id: &str)
    -> Result<Option<LibraryRemoval>, StoreError>;
pub fn pending_library_removals(&self)
    -> Result<Vec<LibraryRemoval>, StoreError>;
pub fn removal_ledger_ids(&self, operation_id: &str)
    -> Result<Vec<String>, StoreError>;
pub fn set_removal_state(&self, operation_id: &str, state: &str, error: Option<&str>)
    -> Result<(), StoreError>;
pub fn clean_library_removal(&self, operation_id: &str)
    -> Result<LibraryRemoval, StoreError>;
pub fn library_accepts_work(&self, library_id: &str) -> Result<bool, StoreError>;
```
- Lifecycle state 借由 removal 表判断；LibraryRemoval 不是独立的另一份扫描事实。

- [ ] **Step 1: 写公开 Store seam 的失败测试。** 在临时 Store 建 A/B 同类型库；A 指向 `/tv`，B 指向 `/tv/kids`；插入两部有效 Media 和两个 ledger。给 A 行写 Tracks 与进度，保存文件/NFO 的字节。调用 begin 后断言冻结但台账尚在；clean 后断言仅 A 台账消失、A Media/进度仍在、B 台账存在、文件逐字节不变。示例测试核心（用真实插入的 row/media/user）：
```rust
let removal = store.begin_library_removal(&a.id).unwrap();
assert!(!store.library_accepts_work(&a.id).unwrap());
assert!(store.ledger_by_path(&a_row.path).unwrap().is_some());
store.set_removal_state(&removal.operation_id, "cleaning", None).unwrap();
let done = store.clean_library_removal(&removal.operation_id).unwrap();
assert_eq!(done.state, "succeeded");
assert!(store.ledger_by_path(&a_row.path).unwrap().is_none());
assert!(store.ledger_by_path(&b_row.path).unwrap().is_some());
assert!(store.get_media(a_row.media_id).unwrap().is_some());
assert_eq!(store.unit_state(user, a_row.media_id, 1, 1).unwrap().unwrap().position_ms, 750_000);
assert_eq!(std::fs::read(&a_row.path).unwrap(), original_video_bytes);
```
本文件 fixture 用 Store::open、create_library、insert_media、insert_ledger、put_file_meta 和 upsert_unit 建立数据，不使用测试专属 SQL替代正常入口。加第二组案例：`/tv` 不匹配 `/tv-old`、目录含 `%/_`、重复根歧义、重复请求返回同 operation、最后同类型库仍 Protected、默认库删除后只提升现存库。
- [ ] **Step 2: 跑红。** `cargo test -p store --test library_removal`；新接口缺失导致失败。
- [ ] **Step 3: 加表与 Store 协议。** app.db 增加表并 bump APP_SCHEMA_VERSION：
```sql
CREATE TABLE library_removals (
 operation_id TEXT PRIMARY KEY, library_id TEXT NOT NULL UNIQUE,
 state TEXT NOT NULL, snapshot_json TEXT NOT NULL,
 removed_ledger_count INTEGER NOT NULL DEFAULT 0,
 error TEXT, created_at_ms INTEGER NOT NULL, updated_at_ms INTEGER NOT NULL
);
```
`snapshot_json` 保存删除前 ledger_id/path/media_id/season/episode 和 roots。begin 在持有 Store 同步锁的调用边界内以严格归属建立快照，预检 last-kind/歧义，app.db 写日志；所有新工作按日志冻结状态拒绝。clean 只接受 `cleaning` 状态（由 Task 3 排空完成后设置），否则返回 Protected；本任务的 Store 测试在没有实际工作租约的 fixture 中显式设置 cleaning 再调用。clean 在 library.db 单事务清 scoped ledger/派生数据；标记仅在同 Media+season+episode 无剩余 ledger 时删除，季对比摘要仅清受影响季，不能清另一库的样本。最后 app.db 单事务删除 roots/library、重选默认、置 succeeded。不要 delete_media 或 delete_playback_for_media。
- [ ] **Step 4: 验证故障恢复。** 给两个事务之间加测试注入断点，清 library.db 后模拟终止，重开 Store 后继续同 operation；重复 clean 不出错，快照计数不因重试变成 0。清理前失败不动文件，库保持 frozen/failed、可重试。不把 failed 库自动激活。历史 probe job 可保留 cancelled 审计终态，不保留自动重试资格；个人观看日志不删。
- [ ] **Step 5: 跑绿并提交。** `cargo test -p store --test library_removal && cargo test -p store`；提交 `feat(store): persist scoped library removal operations`。

## Task 2: 可取消的子进程和工作租约

**Files:**
- Create: `crates/library/src/work_control.rs`, `crates/api/src/library_lifecycle.rs`
- Modify: `crates/library/src/lib.rs`, `crates/library/src/tracks/probe.rs`, `crates/api/src/management/state.rs`
- Modify: `crates/marker/src/fingerprint/capture_types.rs` 和实际启动 ffmpeg 的捕获模块（用 grep `Command` 定位该实现）
- Test: `crates/library/tests/work_control.rs`, `crates/api/tests/library_work_lifecycle.rs`

**Interfaces:**
- Consumes Task 1 `library_accepts_work`。
- Produces library 的依赖中立取消接口和 api 的租约接口：
```rust
#[derive(Clone, Default)]
pub struct WorkControl { cancelled: std::sync::Arc<std::sync::atomic::AtomicBool> }
impl WorkControl {
    pub fn cancel(&self);
    pub fn is_cancelled(&self) -> bool;
    pub fn check(&self) -> Result<(), LibraryError>;
    pub fn output(&self, command: &mut std::process::Command)
        -> Result<std::process::Output, LibraryError>;
}
pub struct LibraryWorkLease { /* library id, control, RAII completion */ }
impl LibraryWorkLease {
    pub fn control(&self) -> &library::WorkControl;
}
```
ApiState 增加共享 `LibraryWorkRegistry`（定义在 library_lifecycle.rs）; `acquire(&self, library_id: &str) -> Result<LibraryWorkLease, String>`、`cancel(&self, library_id: &str)`、`wait_idle(&self, library_id: &str) -> impl Future<Output=()>`。领取和冻结使用同一个 registry mutex 再查 Store；全仓锁序固定 registry → Store，不持有同步 guard 跨 await。每个新请求不能单靠库路径作为身份；租约绑定原 library_id，新库同根也不能解冻旧租约。

- [ ] **Step 1: 写失败测试。** Fake 工作持有 lease，用 channel 挡住发布；cancel 后 `control.check()` 必须返回取消，释放 lease 后 wait_idle 返回；新 acquire 被拒绝。子进程测试启动测试自身的 helper 模式（不依赖 shell sleep），通过 stdout 报 ready，取消后必须 kill+wait，不能遗留 child。
```rust
let control = library::WorkControl::default();
control.cancel();
assert!(control.check().is_err());
```
- [ ] **Step 2: 跑红。** `cargo test -p library --test work_control && cargo test -p api --test library_work_lifecycle`。
- [ ] **Step 3: 实现子进程取消。** 新增 LibraryError::Cancelled；output 使用 spawn、stdout/stderr 并发读取、try_wait 周期检查（例如 50ms），cancel 时 child.kill 再 wait 并 join 输出 reader；不能因输出管道满而死锁。原 probe_tracks_and_duration 委托不取消版本，新增 `probe_tracks_and_duration_controlled(path: &Path, control: &WorkControl) -> Result<(Tracks, Option<i64>), LibraryError>`。指纹捕获请求增加 WorkControl，测试 fake 和生产捕获都检查它；不引入 marker→api 依赖。
- [ ] **Step 4: 实现 registry、RAII 通知并验证。** cancel 设置原所有租约 control；最后一个租约 drop 通知 idle。网络调用使用既有有限 timeout，并在返回后检查取消；删除不能无限等待，无响应任务 30 秒后显示 failed，保持冻结，不能成功返回或提前清数据。
- [ ] **Step 5: 跑绿并提交。** `cargo test -p library && cargo test -p marker && cargo test -p api --test library_work_lifecycle`；提交 `feat: add cancellable library work leases`。

## Task 3: 删除用例协调与所有写入入口隔离

**Files:**
- Modify: `crates/api/src/library_lifecycle.rs`, `crates/api/src/lib.rs`
- Modify: `crates/api/src/http/library_scan.rs`, `crates/api/src/fs_watcher.rs`, `crates/api/src/watch_scrape.rs`, `crates/api/src/watch_intake.rs`
- Modify: `crates/api/src/probe_manager.rs`, `crates/api/src/probe_manager/{probe.rs,queue.rs,recovery.rs}`, `crates/api/src/fingerprint_job/adaptive/capture.rs`
- Modify: `crates/api/src/poster_fetch.rs`, `crates/api/src/auto_resolve.rs`, `crates/api/src/worker/transfer.rs`, `crates/api/src/management/subscribes/run.rs`
- Test: `crates/api/tests/library_removal_races.rs`

**Interfaces:**
- Consumes Task 1 removal protocol、Task 2 lease/control。
- Produces `start_library_removal(state: &ApiState, library_id: &str) -> Result<store::LibraryRemoval, String>` 和 `resume_library_removals(state: &ApiState) -> Result<(), String>`。
- ProbeManager 新增 `cancel_ledger_work(&self, ledger_ids: &[String], reason: &str) -> Result<(), store::StoreError>`：取消 unit 并使关联 marker_refresh 批次不能发布，不取消其他库的独立探测。

- [ ] **Step 1: 写确定性竞态测试。** Fake Catalog/PosterFetch/媒体探测引擎通过 channel 分别停在扫描返回、图片写入、probe 发布之前；开始删除后释放 fake。断言无 ledger/file_meta/marker 复活、无新 sidecar、删除终态只能在租约退出后出现。再用同根目录新库重复扫描，旧 worker 不得写入新库。
```rust
// fake producer 通过 ready/release channel 停在发布边界。
let removal = api::library_lifecycle::start_library_removal(&state, &library.id).unwrap();
// fake 仍未释放时只能处于冻结/排空阶段，不可能完成清理。
let current = state.store().lock().library_removal(&removal.operation_id).unwrap().unwrap();
assert!(matches!(current.state.as_str(), "frozen" | "draining"));
release.send(()).unwrap();
// 释放后使用下述 wait_removal 等待终态，不能断言仍恰好处于 draining。
```
终态等待使用本文件定义的 `async fn wait_removal(state: &ApiState, id: &str) -> store::LibraryRemoval`：tokio::time::timeout(5s, loop 每次查询、非终态 yield/sleep 10ms)，返回 succeeded/failed；channel 测试不依赖实际 30 秒超时。
- [ ] **Step 2: 跑红。** `cargo test -p api --test library_removal_races`。
- [ ] **Step 3: 所有库工作领取租约。** 明确工作顺序：freeze snapshot → cancel registry → 持久 probe 取消/禁止恢复和 due retry → await idle → clean → succeeded。队列 worker 启动前验证原库仍 active、原 ledger_id 仍存在且归属未变；探测/捕获完成后同样验证。批量发布在同一 Store lock 内验证再写，网络和磁盘 IO 不占 Store 锁。sidecar 写入也需租约与发布门禁，不能只阻止 SQL 写。
- [ ] **Step 4: 覆盖非扫描入口和重启。** 检查手动识别/refresh、Watch directory Scrape、Transfer/claim、Playback 自动 enqueue；显式绑定被删库的 Subscribe 保留 binding 并停止目标库执行，最终复制/移动前再次验证目的库。默认库关联的全局工作不批量取消：只取消删库前已经选中该库的 attempt。recovery 在启动扫描和 probe workers 前恢复 removal 并冻结；不得恢复删库里的 queued probe。
- [ ] **Step 5: 跑绿并提交。** `cargo test -p api --test library_removal_races && cargo test -p api --test probe_retry`；提交 `feat(api): drain library work before removing indexed files`。

## Task 4: 删除 HTTP 协议、浏览隔离和 UI

**Files:**
- Create: `crates/api/src/http/library_deletion.rs`
- Modify: `crates/api/src/http/{library_config.rs,mod.rs}`, `crates/api/src/http/library/assets.rs`
- Modify: `crates/store/src/libraries.rs`、浏览/Playback 的 Library visibility 检查
- Modify: `web/lib/api/libraries.ts`, `web/components/library-manage-view.tsx`, `web/components/library-detail-view.tsx`
- Test: `crates/api/tests/management/library_removal.rs`，注册到 management 测试主模块

**Interfaces:**
- Consumes Task 3 start、Task 1 status。
- Produces admin-only endpoints：
```text
DELETE /api/v1/libraries/{id}
202 {"ok":true,"data":{"operation_id":"uuid","state":"draining"}}
GET /api/v1/library-removals/{operation_id}
200 {"ok":true,"data":{"operation_id":"uuid","library_id":"uuid",
     "state":"succeeded","removed_ledger_count":10,"error":null}}
```
重复 DELETE 返回现存操作，成功后的重复 DELETE 可返回该 operation。未知 id 404，last-kind 返回 409/library.protected；若兼容原 400，统一更新测试与前端，不保留两种随机状态码。冻结库的新写操作返回 409/library.deleting。
TypeScript 增加 `LibraryRemoval`、`deleteLibrary(id: string): Promise<LibraryRemoval>`、`getLibraryRemoval(operationId: string): Promise<LibraryRemoval>`。

- [ ] **Step 1: 写 HTTP 失败测试。** 临时建两个 TV 库，扫描含文件的额外库，DELETE 必须 202 而非 protected；状态到 succeeded 后库消失、源文件存在。非管理员 DELETE/GET 无权访问。frozen 后原成员不能访问 items/Playback，默认库不能出现被删库的旧条目；父库删除不影响独立子库。
- [ ] **Step 2: 跑红。** `cargo test -p api --test management library_removal`。
- [ ] **Step 3: 实现路由、重复调用与删除中可见性。** 管理列表可见 removal status，普通可见库过滤冻结项；默认库在删除完成后再提升。既有 Playback/transcode 会话按文件租约关闭，保存最后已接受进度；不再给被删 ledger 发新播放地址。只关闭命中 snapshot 的会话，另一库同 Media 的播放不受影响。
- [ ] **Step 4: UI 等待终态并统一文案。** 三处删除确认统一：
```tsx
const description = "将停止该库相关任务，清除扫描记录与探测缓存。磁盘文件、NFO 和图片不会删除，观看进度、已看和收藏会保留。重新添加目录后将重新扫描。";
```
202 后不立即 toast 成功/跳转。每 1 秒查询 removal，succeeded 才移除卡片、刷新列表/导航；failed 显示 error 和重试按钮。组件卸载停止轮询，重新打开页面从服务端状态继续；去掉“有台账所以不能删除”的 UI 条件，但保留 last-kind 提示。显式绑定删除目标库的 Subscribe 展示“目标 Library 已删除，请重新选择”，不自动解绑。
- [ ] **Step 5: 跑绿并提交。** `cargo test -p api --test management library_removal`；读 web/package.json 执行现有 test/typecheck/build 脚本，不新起替代服务器；提交 `feat(web): expose safe library removal progress`。

## Task 5: NFO 作品身份优先于文件名和在线目录

**Files:**
- Create: `crates/library/src/nfo_identity.rs`, `crates/library/src/watch_identity.rs`
- Modify: `crates/library/src/{lib.rs,nfo.rs,watch.rs}`, `crates/api/src/watch_ledger.rs`, `crates/store/src/media.rs`
- Test: `crates/library/tests/nfo_identity.rs`, `crates/api/tests/nfo_scan_identity.rs`

**Interfaces:**
- Produces 新 library 类型（不污染 Release）：
```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NfoIdentity {
    pub kind: domain::MediaKind,
    pub title: Option<String>,
    pub year: Option<u16>,
    pub original_title: Option<String>,
    pub tmdb_id: Option<String>,
    pub douban_id: Option<String>,
    pub tvdb_id: Option<String>,
    pub bangumi_id: Option<String>,
    pub anilist_id: Option<String>,
}
pub fn parse_nfo_identity(body: &str) -> Option<NfoIdentity>;
pub fn read_watch_identity(path: &std::path::Path, root: &std::path::Path,
    kind: domain::MediaKind) -> Result<Option<NfoIdentity>, crate::LibraryError>;
```
TransferredFile 增加 `nfo_identity: Option<NfoIdentity>`。读取失败记 warn 并安全 fallback；XML root 不匹配媒体类型则不采用。`episodedetails` 不返回 TV Media 身份，只补分集 season/episode。
Store 新增 `resolve_scanned_media(&self, incoming: domain::Media) -> Result<domain::Media, StoreError>`：先可靠 kind+外部 ID，再无冲突 title/kind/year；冲突返回 Protected、不写错进度、不覆盖旧别名。record_paths 使用它，不先 title lookup 再跳过 ID。

- [ ] **Step 1: 写解析红测试。** 使用实际 XML，不 mock private helper：
```rust
let xml = r#"<tvshow><title>动灵守护者</title><year>2022</year>
<uniqueid type="tmdb" default="true">207890</uniqueid></tvshow>"#;
let identity = library::parse_nfo_identity(xml).unwrap();
assert_eq!(identity.kind, domain::MediaKind::Tv);
assert_eq!(identity.tmdb_id.as_deref(), Some("207890"));
assert!(library::parse_nfo_identity(
    "<episodedetails><title>第一集</title><uniqueid type=\"tmdb\">999</uniqueid></episodedetails>"
).is_none());
```
增加 `<tmdbid>`/typed uniqueid、movie/tv 数字相同、空/非法数字、矛盾 ID、只有 `<id>` 未声明来源不猜 TMDB、episode NFO season/episode 冲突案例。
- [ ] **Step 2: 跑红。** `cargo test -p library --test nfo_identity && cargo test -p api --test nfo_scan_identity`。
- [ ] **Step 3: 实现有边界的 NFO 查找。** movie 优先同 basename.nfo，其次同目录 movie.nfo，不选任意兄弟作品 NFO；TV 从 episode 所在目录向 Library root 搜 tvshow.nfo，不越出 root、不跟目录 symlink 越界。episode.nfo 只提供集元数据，tvshow.nfo 提供作品元数据。有效 NFO 覆盖文件名标题/年份，即使 filename confidence=High；无法形成可靠作品身份时保留 filename/目录 fallback。不要用 episode 年份代替 TV 首播年份。
- [ ] **Step 4: 实现可靠 Media 复用并测冲突。** 同外部 ID+kind 复用旧 MediaId，允许中英文标题变化；不同外部 ID 但标题相同不可合并；同类型 NFO 不同 provider 分别命中两个旧 Media 时返回冲突，不挑第一个。无外部 ID 只在唯一且无已知别名冲突的 title/kind/year 下复用；不承诺所有无 ID 作品进度恢复。新行 title 来自合法 NFO；Media::merge 不允许修改已有内部 id。
- [ ] **Step 5: 跑绿并提交。** `cargo test -p library && cargo test -p store && cargo test -p api --test nfo_scan_identity`；提交 `feat(scan): resolve canonical media from local nfo identity`。

## Task 6: 避免 NFO 重建时重复联网，修复在线识别身份汇合

**Files:**
- Modify: `crates/api/src/auto_resolve.rs`, `crates/api/src/http/library_scan.rs`, `crates/api/src/watch_ledger.rs`
- Test: `crates/api/tests/nfo_scan_identity.rs`, `crates/api/tests/management/auto_resolve_scan.rs`

**Interfaces:**
- Consumes Task 5 NfoIdentity 和 resolve_scanned_media。
- Produces scan 的本地元数据分支，不另建第二份 NFO 数据库：身份/标题已确定且 local NFO 可用时不 catalog search、不强制 fetch details、不重写 existing NFO；图片已存在不再次下载。显式 refresh 保留主动联网更新能力。
- 新 Store 接口 `rebind_scanned_ledger(&self, ledger_id: domain::LedgerId, media_id: domain::MediaId) -> Result<(), StoreError>`，仅迁移本次扫描产生的 ledger；不删除/合并用户历史记录。auto_resolve 匹配到旧别名时把新 ledger 指向旧 Media，后续刷新重新读取 canonical Media；不单纯 update 新 Media 的 tmdb_id。

- [ ] **Step 1: 写失败测试。** fake Catalog 在任何 search/details 被调用时返回错误并记录请求；文件名为英文 High，tvshow.nfo 中文含可靠 ID，旧 Media 中文不同别名标题但同 ID。扫描成功后 ledger 指向旧 Media，零 search/details 请求，NFO/图片字节不变。相反残缺 NFO 无可靠身份时 fake Catalog 返回唯一正确结果，允许联网补齐。
- [ ] **Step 2: 跑红。** `cargo test -p api --test nfo_scan_identity`。
- [ ] **Step 3: 实现分支与身份汇合。** 将 scan 的“识别”、“媒体探测”和“写 Scrape 侧车”分开：NFO 完整指身份和本地可展示标题存在，不要求每项 cast/plot 都有；不因简介缺失隐式重抓全套 metadata。无本地可靠身份但在线找到已有外部 ID时复用旧 MediaId再绑定本次台账。未匹配时仍保留本次文件事实，但不臆造历史身份。
- [ ] **Step 4: 验证仍做实际媒体探测。** 插入新 ledger 后检查持久 media_probe job 已建立且不 force_fingerprint；fake 媒体探测输出 Tracks/时长，确认写新 ledger 的 file_meta。STRM fixture 不请求真实源；NFO runtime/streamdetails 不让新 ledger 探测误判 Complete。新库两个检测开关均 false。
- [ ] **Step 5: 跑绿并提交。** `cargo test -p api --test nfo_scan_identity && cargo test -p api --test management auto_resolve_scan`；提交 `fix(scan): reuse nfo metadata and canonical playback identity`。

## Task 7: 删除 → 重建 → 观看进度恢复端到端验收

**Files:**
- Create: `crates/api/tests/library_delete_reimport.rs`, `docs/adr/0012-library-removal.md`
- Modify: `docs/testing/system-regression-cases.md`

**Interfaces:**
- Consumes Tasks 1–6 DELETE/status、NFO 识别、MediaId 复用。
- Produces 可复现公开 API 验收与设计记录，不添加新的行为。

- [ ] **Step 1: 写端到端红测试。** fixture 建 extra TV Library、旧 Media（tmdb_id）、两用户进度（S01E03 为 750000/120000ms，收藏各自不同）、本地 tvshow.nfo+episode STRM+图片、probe cache/stage。通过公开 DELETE/status 等待 succeeded，再 POST 同根创建新库、公开 scan；检查：
```rust
assert_ne!(new_ledger.id, old_ledger.id);
assert_eq!(new_ledger.media_id, old_ledger.media_id);
assert_eq!(store.unit_state(user_a, new_ledger.media_id, 1, 3).unwrap().unwrap().position_ms, 750_000);
assert_eq!(store.unit_state(user_b, new_ledger.media_id, 1, 3).unwrap().unwrap().position_ms, 120_000);
assert!(store.get_file_meta(&old_ledger.id.to_string()).unwrap().is_none());
assert!(store.get_fingerprint_cache(&old_ledger.id.to_string()).unwrap().is_none());
```
fixture 自己通过公开 Store/API 数据写入，fake worker 不访问网络；确认新 file_meta 来自新 probe 而非旧 ledger 复制。增加 movie 整片 progress、不同 TMDB ID 同名不恢复、NFO 无 ID 歧义不自动合并、另一库同 Media 留存、删除默认但非最后一个库、restart mid-clean 与 restart draining、成员可见性和 cancelled job 不恢复案例。
- [ ] **Step 2: 跑红。** `cargo test -p api --test library_delete_reimport`；如果已经全绿，暂时回退具体 identity/gate 分支验证测试确实能捕捉错误，不提交该临时回退。
- [ ] **Step 3: 写 ADR 与回归说明。** ADR 记录清入库事实不清用户历史、最后库保护、任务取消/发布栅栏、四库幂等恢复、NFO episode ID 不等于 TV ID、无 ID 不保证恢复、绑定删除目标库的 Subscribe 不默认重定向。记录 Emby/Jellyfin 可核验的“移除库不删除磁盘文件”来源，不声称其内部缓存/进度实现已确认：Jellyfin https://forum.jellyfin.org/t-delete-a-library-without-deleting-files?pid=71881&mode=linear；Emby https://emby.media/community/topic/117044-delete-library/。
- [ ] **Step 4: 总验证与边界审查。**
```bash
cargo test -p library
cargo test -p marker
cargo test -p store
cargo test -p api
cargo test --workspace
```
所有命令保留真实退出码；输出重定向时读取失败详情，不用 `| tail` 隐藏 cargo 失败。执行 web 既有 test/typecheck/build；逐文件审 diff，没有 filesystem remove_file/remove_dir 用于用户 Library roots；检查原有 static artwork 公共路由未动。对修改 Rust 文件执行 wc -l并检查函数硬限。使用 Channel/Barrier 控制竞态，不用任意睡眠宣称取消完成。
- [ ] **Step 5: 提交并交付。** 提交 `test: verify library removal and playback-preserving reimport`；最终答复提供提交号、公开行为、验证结果和“无可靠身份不会强行恢复旧进度”的限制。

## 自审覆盖清单

- [x] 含台账删库允许，磁盘文件/侧车不动：Tasks 1、3、4、7。
- [x] 扫描/探测/声纹取消，旧任务不能复活数据、不能写新库：Tasks 2、3、7。
- [x] 不碰嵌套库/共享 Media，路径边界和歧义保护：Tasks 1、4、7。
- [x] 四数据库中途失败、重启恢复与重复请求：Tasks 1、3、7。
- [x] NFO 优先、作品和单集身份分离、无真实网络：Tasks 5、6。
- [x] 清文件事实、保留 Media和两用户 Playback，再扫描认回 MediaId：Tasks 1、5、6、7。
- [x] 媒体流仍探测；新库 IntroDB/章节/声纹仍关闭：Tasks 3、6、7。
- [x] Subscribe 不删、目标失效明确提示，不静默转移默认库：Tasks 3、4、7。
- [x] last-kind 不变、默认库提升、管理删除中状态与端点权限：Tasks 1、4。
- [x] 本计划仅文档，不执行删除用户实际 Library、不更改现有生产记录。

## 执行交接

按 Task 1 → 2 → 3 → 4 → 5 → 6 → 7 顺序执行，每个任务各自 red/green/review/commit。Tasks 1–4 提供安全删库的纵向切片；Tasks 5–7 完成可靠重新扫描和观看数据恢复，整体验收后才宣称本需求完成。执行前记录当前 HEAD 和未提交文件；不把其他会话的代码修改带入提交。
