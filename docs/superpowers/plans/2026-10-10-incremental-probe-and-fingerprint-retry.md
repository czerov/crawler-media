# 媒体信息与声纹增量探测优化 Implementation Plan

> **历史状态（2026-10-13 整理）：** 已有实现：5eaf6b6、3be7c39、937276d、c5275c0 提供持久化阶段/重试资格、缺失声纹增量入队、截止时间重试与等待期停止轮询。真实远端 403/EOF 根因、整季性能和真实重启/播放器体验不因实现提交而视为已验证。
>
> 本文保留原设计、发现/任务 ID 与当时证据；日期本身不表示过时。旧执行/提交/清理命令仅作历史记录，不应直接重跑；复选框不是当前实现清单。当前依据：[数据库/API review](<../../audits/2026-10-13-database-api-review.md>) 与 [剩余计划](<2026-10-13-database-api-followup.md>)，未覆盖的旧 ID 仍须单独对账。

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 恢复单集时只处理该集的缺失事实；片尾声纹失败后保留已成功结果，由后台按持久化退避补采，不因查询轮询不断创建任务或重复整季比对。

**Architecture:** 保留现有 ProbeManager 的媒体信息高优先级队列、声纹低优先级队列及 probe_jobs 历史，不迁移整个 Job 系统。新增 SQLite 阶段事实及重试资格，以 ledger、来源版本、采集参数为边界统一自动入队；已有后台 tick 派发到期工作，不新增私有定时器。比对通过确定性的整季输入摘要去重，只有有效输入变化才重新发布结果。

**Tech Stack:** Rust workspace、Tokio、rusqlite/SQLite、tracing、axum；React 19 / TypeScript、Node 原生测试、pnpm。

## Global Constraints

- 实施用户当前请求，不创建 GitHub issue，不扩展为全库缓存重构。
- 术语遵循 CONTEXT.md：Media、Library、Ledger、Job、Playback、Scrape、NFO。
- SQLite 是状态事实来源；内存集合仅作为执行中去重优化，不能决定重启后的重试资格。
- 不新增后台服务、独立数据库或私有 retry 定时线程；接入现有后台 tick。
- TDD：先通过公共入口写失败测试，再实现；测试不得访问真实远程视频、TMDB、Downloader 或 Chromium。
- 每个触及 crate 执行 `cargo test -p <crate>`，最终执行一次 `cargo test --workspace`。
- `.rs` 文件硬上限 800 行，生产函数/方法和测试函数硬上限 120 行；超过软限制应按职责评审。
- 只提交本次修改。开始执行前记录工作区已有修改，不覆盖、不暂存、不提交用户的无关改动。
- 日志：意外失败及第三方错误必须保留 error 上下文；退避/降级 warn；任务生命周期 info；逐文件缓存命中和匹配细节 debug。
- 不修改静态图片端点认证，不修改 Playback 协议认证。
- 本文件保留最初实施方案；后续已实现（见顶部提交），但原计划中的命令/预期不自动成为实际测试或真实来源验收证据。

---

## 1. 调查证据与边界

### 1.1 本次日志实际行为

- `.test-logs/backend.log:379-386`：16:37:14 删除 E08，16:38:03 超过 45 秒宽限并删除事实，`remaining=7`。
- `:394-408`：恢复后解析 8 个文件名，`scanned=8 inserted=1`，只对 E08 实际执行媒体信息探测（1940ms）。
- `:581,699,825,943`：E08 后续媒体信息命中版本缓存，均为 0ms。
- `:503,621,747,865,995`：片头采集一次；片尾采集五次，前四次失败，最后成功。
- `:496-501,598-618`：片尾读取出现 403/EOF；不能据此认定签名或 Range 的具体根因。
- `:506,624,750,868,997`：每次声纹处理又比对整季缓存；这不是重新读取全季视频。
- E06 另有三次远程 404，不应归因于 E08 删除。

### 1.2 已确认根因

1. `probe_manager/probe.rs:572-582` 对普通任务的片尾失败返回成功。
2. `probe_manager.rs:415-430` 只在失败时记录冷却，部分失败因此绕过冷却。
3. `http/library/probe.rs:37-98` 查询发现片尾缺失后再次从媒体信息入口入队。
4. `probe_manager/probe.rs:147-156` 不判断声纹输入是否变化，直接整季比对。
5. `marker_resolver.rs:202-209` 使用 detect_intros，后台声纹却仅判断 enable_fingerprint。

### 1.3 本次不做

- 不实现删除后跨 LedgerId 复用采集缓存；超宽限恢复 E08 仍可重新采集 E08。
- 不修改 STRM source_version 算法，不把稳定 URL 等同于稳定远端内容。
- 不实现复杂的局部 pair 匹配增量算法；先消除相同输入重复比对。
- 不调整 FFmpeg 403/seek 策略；记录真实错误类型与上下文即可。
- 不修改文件删除宽限期，不改变手动整季刷新“全部成功后原子替换”的语义。
- 不借机修复已发现的现有 STRM URL 修改主动入队缺口；但所有进入本次统一入口的新来源版本必须正确失效。

## 2. 行为规格（执行时不得另行猜测）

### 2.1 自动决策

| 条件 | 动作 |
|---|---|
| 来源版本有效，媒体信息有效，声纹完整 | 不创建任务 |
| 媒体信息无效 | 排媒体信息；完成后按当前开关安排声纹 |
| 媒体信息有效，仅声纹缺失 | 直接排声纹；不创建 media_probe |
| 某阶段失败且未到 next_retry_at_ms | 返回等待状态，不创建新任务 |
| 某阶段失败且到期 | 后台认领一次，只执行有资格的缺失阶段 |
| 自动重试耗尽 | 不自动重试；明确显示可手动重试 |
| 文件删除 / 台账移除 | 取消待执行工作；旧执行结果不得发布 |
| 来源版本或采集参数改变 | 新上下文重新评估；旧失败不能阻止新版本，旧完成不能覆盖新版本 |

### 2.2 状态与重试

- 阶段：`metadata`、`intro`、`outro`。
- 阶段状态：`pending`、`queued`、`running`、`succeeded`、`failed`、`not_applicable`、`cancelled`。
- 声纹任务终态增加 `partial`：有有效片头而片尾失败；`partial` 不 active，不等同完整成功。
- 自动重试：初次失败后等待 60 秒；其后失败分别等待 300、900、3600 秒；第五次失败停止自动重试（初次 + 最多四次重试）。
- 错误类别：`http_403`、`http_404`、`timeout`、`network`、`empty_audio`、`source_deleted`、`source_changed`、`store`、`unknown`。
- 403/404 同样有上限与退避，不立即永久失败；`source_deleted/source_changed` 取消旧上下文；`store` 错误不得发布成功或继续无约束自动执行。
- 时间统一 Unix 毫秒 `i64`，生产 clock 读取系统时间，测试注入 FakeClock，不使用真实等待。
- 手动“重试失败阶段”可跳过等待并重置所选失败阶段的连续失败计数，但仍复用成功缓存、受执行去重约束。
- 现有手动强制媒体刷新/整季刷新保留强制语义；不得把普通补采升级为强制刷新。
- 未知媒体时长意味着暂不能决定片尾窗口，不得误记 `not_applicable`；重新获取可靠时长进入 metadata 失败/退避路径，避免成功却永远缺片尾。
- 已知时长 <= `(sample_duration_secs + 30) * 1000` 时片尾为 `not_applicable`。

### 2.3 开关

- 自动声纹资格：`kind == Tv && detect_intros && enable_fingerprint`。
- detect_intros 关闭：不自动生成、不自动比对/发布，保留已有标记。
- enable_fingerprint 关闭：不自动生成声纹，不影响总开关下其他标记来源。
- 不在关闭总开关时清除原有 enable_fingerprint 值；重新开启后恢复用户选择。
- 队列入队、执行前和发布前检查当前开关；关闭期间完成的采集可保留缓存，但不得发布自动标记。
- 手动识别也遵守开关；关闭时现有手动接口返回 409 和明确错误码 `library.marker_detection_disabled`，不隐式绕过。
- 关闭开关不消耗失败次数；重新开启仍遵守此前尚未到期的退避。

### 2.4 比对

- 确定性输入摘要包括：MediaId/season、排序后的成员 LedgerId/来源版本、实际有效 intro/outro 内容摘要及尾窗偏移、匹配算法版本和有效匹配参数。
- 必须包含“无有效样本”的成员，保证删除/新增及版本变化可见；不得把 job_id、时间戳、SQL 返回顺序混入摘要。
- 与最近成功发布摘要相同：跳过匹配和标记/章节写入。
- 摘要不同：执行现有整季算法；发布成功后记录摘要，失败不能更新摘要。
- 发布前重新验证成员与来源版本；事务内提交结果与成功摘要，旧快照不得覆盖新结果。
- 首次 E08 片头成功：比对一次；后续三次片尾失败且输入未变：零次；最后片尾成功：比对一次。
- 删除 E08 不主动生成声纹；有关季模型现有失效语义不变。下次合法比对请求用成员摘要识别变动。

## 3. 文件结构与职责

新模块均需在既有模块树注册；不要把所有功能塞回大型文件。

| 路径 | 变更与职责 |
|---|---|
| `crates/store/src/probe_state.rs` | 新建；阶段类型、状态读取和 CAS 更新 |
| `crates/store/src/probe_state/retry.rs` | 新建；退避、到期候选、原子认领 |
| `crates/store/src/schema/probe_state.rs` | 新建；阶段状态/成功比对摘要 DDL 与幂等迁移 |
| `crates/store/src/schema.rs`, `crates/store/src/lib.rs` | 注册 schema、导出公共类型；library schema 当前 6，若仍为 6 升 7，否则递增实际值 |
| `crates/store/src/probe_tasks.rs` | 扩展 partial 聚合与恢复，不破坏现有强制批次 |
| `crates/store/src/library.rs` | 台账删除时清理新阶段状态，不能影响其他集 |
| `crates/store/src/library/markers.rs` | 成功摘要与标记结果同事务提交，沿用现有 marker 持久化模块 |
| `crates/store/tests/probe_stage_state.rs` | 新建；状态、迁移、重启、认领竞态测试 |
| `crates/api/src/probe_manager/policy.rs` | 新建；资格、版本缓存、统一入队决策 |
| `crates/api/src/probe_manager/retry.rs` | 新建；到期派发与错误分类；复用已有 worker |
| `crates/api/src/probe_manager/stages.rs` | 新建；阶段执行结果持久化与部分成功聚合 |
| `crates/api/src/probe_manager/comparison.rs` | 新建；规范化输入摘要、去重与安全发布协调 |
| `crates/api/src/probe_manager.rs` | 注册模块、构造器注入 clock/probe seam，精简 enqueue 决策 |
| `crates/api/src/probe_manager/{queue,worker,probe,recovery,markers}.rs` | 按阶段路由；复用缓存；恢复直接声纹任务；调用摘要发布 |
| `crates/api/src/fingerprint_job/cache.rs` | 返回阶段结果及样本变更；保留成功采集缓存 |
| `crates/api/src/probe_manager/season.rs` | 自适应入口遵守相同开关、重试与发布安全约束 |
| `crates/api/src/http/library/probe.rs` | 所有自动入口使用统一 policy；显式手动失败阶段重试 |
| `crates/api/src/job_loop.rs` | 在已有 tick 中派发到期 probe，不新增 loop |
| `crates/api/src/http/{library,library_chapters,library_config}.rs` | 分阶段 DTO、手动开关拒绝、配置变更提示 |
| `crates/api/src/marker_resolver.rs` | 复用总开关策略，修正过时的一次拉流注释 |
| `crates/api/tests/probe_retry.rs`, `crates/api/tests/probe_retry/support.rs` | 新建；公开管理器/HTTP 边界的确定性回归及 fixtures |
| `web/lib/api/libraries.ts` | 阶段 DTO、partial 与重试结果兼容 |
| `web/lib/probe-status.ts` | 新建；纯展示状态选择器 |
| `web/components/library-item-probe-status.tsx` | 新建；阶段状态及失败重试按钮，避免继续增大详情组件 |
| `web/components/library-item-detail-view.tsx` | 接入状态组件，等待退避不再持续刷新整条详情 |
| `web/test/probe-status.test.mjs` | 新建；纯状态展示/轮询决策测试 |
| `docs/adr/0011-probe-stage-retry.md` | 新建；记录既有 probe_jobs 的阶段演进及 ADR-0009 的受限例外 |

同事务发布方法扩展现有 `crates/store/src/library/markers.rs`，不建立平行的 marker_results 模块。

## 4. 统一接口契约

以下是本计划新增接口；后续任务必须沿用名称和语义。完整类型和方法在对应任务实现，不能在调用点各造一套。

```rust
// store::probe_state
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeStage { Metadata, Intro, Outro }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeStageStatus {
    Pending, Queued, Running, Succeeded, Failed, NotApplicable, Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProbeStageKey {
    pub ledger_id: String,
    pub context_key: String,
    pub stage: ProbeStage,
}

#[derive(Clone, Debug)]
pub struct ProbeStageState {
    pub key: ProbeStageKey,
    pub status: ProbeStageStatus,
    pub failure_count: u32,
    pub next_retry_at_ms: Option<i64>,
    pub error_kind: Option<String>,
    pub error: Option<String>,
    pub active_job_id: Option<String>,
}

#[derive(Clone, Debug)]
pub enum ProbeStageCompletion {
    Succeeded,
    Failed { error_kind: String, error: String },
    NotApplicable,
    Cancelled { reason: String },
}

// API 只传上下文/Job身份，不执行 SQL。
// Store 方法全部返回 Result<_, StoreError>。
// get_probe_stage(&self, key: &ProbeStageKey) -> Result<Option<ProbeStageState>, StoreError>
// finish_probe_stage(&self, key: &ProbeStageKey, job_id: &str,
//     completion: &ProbeStageCompletion, now_ms: i64) -> Result<bool, StoreError>
// bool=false 表示来源已换、任务取消或执行身份过期，调用者不得发布。
// list_due_probe_stages(&self, now_ms: i64, limit: usize)
//     -> Result<Vec<ProbeStageState>, StoreError>
// retry_delay_ms(failure_count: u32) -> Option<i64>

// api::probe_manager::policy（公开供 HTTP 和集成测试使用）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeRequestOrigin {
    Filesystem, Detail, Playback, BackgroundRetry, ManualRetry, ManualRefresh,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProbeRequestResult {
    Complete, Queued { job_id: String }, AlreadyRunning,
    Waiting { next_retry_at_ms: i64 }, Exhausted, Disabled, Cancelled,
}
// ProbeManager::request_probe(&self, row: &LedgerRow, origin: ProbeRequestOrigin)
//     -> Result<ProbeRequestResult, StoreError>
// ProbeManager::dispatch_due(&self, now_ms: i64, limit: usize)
//     -> Result<usize, StoreError>
// Clock::now_ms(&self) -> i64; Clock: Send + Sync
```

上下文 key：metadata 使用 source_version；intro/outro 使用当前 fingerprint_cache_key（包含来源、算法、采样和可靠时长）。阶段状态仅描述当前上下文，旧上下文保留 Job 历史、清除其自动重试资格；不能按 ledger_id 单独判断成功。

## 5. 实施任务

### Task 1: SQLite 阶段状态、退避与 partial 兼容

**Files:** 新建 store probe_state、retry、schema 模块和 `crates/store/tests/probe_stage_state.rs`；修改 schema/lib/probe_tasks/library 模块；新增 ADR。

**Consumes:** 既有 Store、probe_jobs/probe_job_units、LedgerId 和迁移机制。

**Produces:** 第 4 节的 Store 类型、读取/完成/到期方法；`retry_delay_ms`；阶段认领与 Job 创建在同一 library.db 事务内的内部方法。

- [ ] **Step 1: 写失败测试。** 以临时 Store 和插入的 LedgerRow 为 fixture，通过公开 Store 方法验证失败状态持久化、重复认领和重新打开数据库。退避纯函数测试使用以下精确断言：

```rust
#[test]
fn retry_schedule_has_four_delays_then_stops() {
    use store::retry_delay_ms;
    assert_eq!(retry_delay_ms(1), Some(60_000));
    assert_eq!(retry_delay_ms(2), Some(300_000));
    assert_eq!(retry_delay_ms(3), Some(900_000));
    assert_eq!(retry_delay_ms(4), Some(3_600_000));
    assert_eq!(retry_delay_ms(5), None);
}
```

另外写 `partial_job_is_terminal_after_reopen`、`stale_job_cannot_finish_new_context`、`two_connections_claim_due_stage_once`、`deleting_ledger_removes_only_its_stage_state`、`v6_database_migrates_without_losing_probe_jobs`。测试断言持久化行和返回值，不断言 SQL 文本。

- [ ] **Step 2: 跑红。** `cargo test -p store --test probe_stage_state`；初次应因新接口缺失或旧成功状态不符合断言失败。
- [ ] **Step 3: 实现 schema 和状态机。** 阶段表以 `(ledger_id, context_key, stage)` 为主键，字段覆盖第 4 节并增加 updated_at_ms；retry 索引覆盖 failed/next_retry_at_ms。使用事务校验 ledger、当前上下文和 active_job_id；只有匹配身份能完成。`retry_delay_ms` 实现如下：

```rust
pub fn retry_delay_ms(failure_count: u32) -> Option<i64> {
    match failure_count {
        1 => Some(60_000),
        2 => Some(300_000),
        3 => Some(900_000),
        4 => Some(3_600_000),
        _ => None,
    }
}
```

partial 聚合只用于普通补采；强制原子批次包含任一不完整阶段仍失败、保持旧标记。旧 succeeded 历史不能当作缓存完整依据；首次需要时根据有效缓存初始化阶段，禁止升级时全量拉流。所有识别终态的 SQL/序列化处理必须包含 partial；active 仍仅 queued/running。

- [ ] **Step 4: 跑绿及尺寸检查。** `cargo test -p store`；`wc -l` 检查触及 Rust 文件；补迁移重复运行及旧 queued/running 恢复测试。
- [ ] **Step 5: 文档及提交。** ADR 写明 probe_jobs 是既有实现，不引入第三套 scheduler；当前阶段继续放 library.db，与 ledger 同生命周期，后续全量统一 Job 存储不是本次目标。仅暂存本任务文件，提交 `feat(store): persist probe stage outcomes and retry eligibility`。

### Task 2: 统一资格判断，直接投递缺失声纹

**Files:** 新建 policy/stages；修改 manager、queue、worker、probe、recovery、HTTP probe 和 fingerprint cache；新建 API 回归及 support。

**Consumes:** Task 1 阶段状态；现有 FingerprintEngine 和缓存 key。

**Produces:** `request_probe`、`ProbeRequestResult`、`ProbeRequestOrigin`；直接声纹队列恢复；真实阶段状态写入。

- [ ] **Step 1: 建立公共 seam 的失败测试。** 在 support 中定义 `ProbeScenario`，构造临时 Library、Media、E01–E08 的真实台账/STRM 和预置缓存。它暴露：

```rust
// 这些是本任务创建的测试 fixture 接口，非生产接口。
// ProbeScenario::cached_season() -> Self
// missing_outro(&mut self, episode: u32)
// fail_next_outro(&mut self, episode: u32, error: &str)
// request(&self, episode: u32, origin: ProbeRequestOrigin) -> ProbeRequestResult
// drain(&mut self).await   // 等待受控 fake 完成；设测试超时，无 sleep
// metadata_reads(&self, episode: u32) -> usize
// intro_reads(&self, episode: u32) -> usize
// outro_reads(&self, episode: u32) -> usize
// fingerprint_job_status(&self, episode: u32) -> String
```

使用 fake MediaProbe 记录实际视频读取，fake FingerprintEngine 按 `(path,start)` 返回固定非空样本或指定失败；不调用本机 ffprobe/ffmpeg。若现有媒体信息路径没有 trait，新增仅覆盖 `probe_tracks_and_duration` 的 `MediaInfoProbe` 注入，生产适配器仍调用现有 library 方法。不得为测试更换主执行逻辑。

```rust
#[tokio::test]
async fn missing_outro_skips_metadata_and_preserves_intro() {
    let mut s = ProbeScenario::cached_season();
    s.missing_outro(8);
    s.fail_next_outro(8, "HTTP error 403 Forbidden");
    assert!(matches!(s.request(8, ProbeRequestOrigin::Detail),
        ProbeRequestResult::Queued { .. }));
    s.drain().await;
    assert_eq!(s.metadata_reads(8), 0);
    assert_eq!(s.intro_reads(8), 0);
    assert_eq!(s.outro_reads(8), 1);
    assert_eq!(s.fingerprint_job_status(8), "partial");
}
```

再写 `cached_complete_episode_creates_no_job`、`concurrent_queries_share_one_fingerprint_job`、`restart_resumes_fingerprint_without_metadata_job`、`unknown_duration_is_not_outro_not_applicable`。真实外部读取次数是本 bug 的可观察 seam，允许断言；不要断言内部 helper 调用次数。

- [ ] **Step 2: 跑红。** `cargo test -p api --test probe_retry missing_outro_skips_metadata_and_preserves_intro`；旧逻辑应显示媒体信息任务链或错误 succeeded。
- [ ] **Step 3: 实现最小路由。** policy 同时验证来源版本、缓存流信息和可靠时长。request 统一检查数据库重试资格及 active 状态；创建 Job 与阶段认领一个事务完成；发送失败将阶段标失败并保留错误。已有有效媒体信息只建 fingerprint_probe，并携带当前时长/source_version。

阶段采集结果使用枚举而非 bool：

```rust
pub enum FingerprintRunOutcome {
    Complete,
    Partial { error_kind: String, error: String },
    Failed { error_kind: String, error: String },
    Cancelled { reason: String },
}
```

成功片头必须在片尾执行前安全持久化，避免片尾时崩溃丢掉已完成阶段。缓存写入验证当前 ledger、source_version、context_key 和 job 身份；每次外部读取后及发布前再验证。恢复 fingerprint_probe 直接入声纹队列，不能一律放回 metadata。旧 unit bool 字段仅保留兼容映射，自动路径不再用 force_fingerprint 表示“缺失”。

- [ ] **Step 4: 跑绿。** `cargo test -p api --test probe_retry`，`cargo test -p api`。验证媒体优先级 gate、旧强制刷新、删除取消回归不变。
- [ ] **Step 5: 提交。** `fix(api): route missing fingerprint stages without metadata reprobes`，只暂存本任务新增及修改内容。

### Task 3: 到期后台重试，不依赖查询刷新

**Files:** 新建 retry 模块；修改 policy、worker、job_loop、recovery；扩展 store 原子认领测试和 probe_retry。

**Consumes:** Task 1 持久化资格、Task 2 request 路由。

**Produces:** `dispatch_due(now_ms, limit)`、Clock 注入；自动四次重试上限及显式手动失败阶段重试。

- [ ] **Step 1: 写失败测试。** support 增加 `set_time_ms(i64)`、`dispatch_due() -> usize`、`reopen()`；时间零点固定 1_000_000。验证不访问详情也能到期重试，反复访问也不能绕过退避：

```rust
#[tokio::test]
async fn queries_cannot_bypass_outro_retry_deadline() {
    let mut s = ProbeScenario::cached_season();
    s.set_time_ms(1_000_000);
    s.missing_outro(8);
    s.fail_next_outro(8, "HTTP error 403 Forbidden");
    s.request(8, ProbeRequestOrigin::Detail);
    s.drain().await;
    for _ in 0..20 {
        assert_eq!(s.request(8, ProbeRequestOrigin::Detail),
            ProbeRequestResult::Waiting { next_retry_at_ms: 1_060_000 });
    }
    s.reopen();
    s.set_time_ms(1_059_999);
    assert_eq!(s.dispatch_due(), 0);
    s.set_time_ms(1_060_000);
    assert_eq!(s.dispatch_due(), 1);
    s.drain().await;
    assert_eq!(s.metadata_reads(8), 0);
    assert_eq!(s.intro_reads(8), 0);
    assert_eq!(s.outro_reads(8), 2);
}
```

另外实现 `fifth_failure_exhausts_automatic_retries`、`manual_retry_reuses_successful_stages`、`new_source_does_not_inherit_old_backoff`、`deleted_ledger_is_never_retried`、`late_attempt_cannot_finish_new_attempt`。手动 retry 与到期 tick 同时请求只允许一个采集。

- [ ] **Step 2: 跑红。** `cargo test -p api --test probe_retry queries_cannot_bypass_outro_retry_deadline`。
- [ ] **Step 3: 接入已有 tick。** job_loop 每轮首先以系统 tick 时间换算毫秒，调用 `state.probe.dispatch_due(now.saturating_mul(1000), 32)`，只认领/投递，不等待媒体读取；失败 error 记录完整上下文。到期查询过滤当前 ledger、当前上下文、开关和活动 Job；认领用 Task 1 事务，重叠 tick 不重复创建。构造器恢复只恢复在途身份，不把等待退避的 failed 状态当 queued。

自动 retry 只认领失败且到期阶段；相邻阶段仍未到期时不得顺带补采。显式重试失败阶段调用 request 的 ManualRetry，不清成功缓存；执行中的请求返回 AlreadyRunning。

- [ ] **Step 4: 跑绿。** `cargo test -p store`、`cargo test -p api`。重启测试重建 Store/Manager，不能只清空内存 map 模拟。
- [ ] **Step 5: 提交。** `fix(api): dispatch bounded probe retries from persisted deadlines`。

### Task 4: 相同输入不重复比对，变化后安全发布

**Files:** 新建 comparison；修改 markers、probe、season 相关入口；store 增加成功摘要持久化及同事务发布；扩展 probe_retry。

**Consumes:** Task 2 完整/部分采集结果，现有季匹配器和版本安全发布能力。

**Produces:** `SeasonComparisonInput` 规范化摘要；相同输入跳过匹配/写入；成功发布与摘要原子提交。

- [ ] **Step 1: 写失败测试。** support 增加 `comparison_runs() -> usize`、`published_comparison_digest() -> Option<String>`；通过注入匹配器适配器观察真实比对边界，必须同时断言保存的标记/章节未变化。先片头新增成功且片尾失败，再片尾重复失败，最后成功：

```rust
// 连续三轮请求，第二轮通过 FakeClock 前进到重试时间。
// 第一轮新增片头 -> 比对 1 次；第二轮无新样本 -> 仍 1 次；
// 第三轮新增片尾 -> 总共 2 次；最终 E08 标记两端都存在。
assert_eq!(s.comparison_runs(), 2);
assert!(s.published_comparison_digest().is_some());
```

另写 `comparison_digest_ignores_row_order_and_job_identity`、`membership_or_matching_config_changes_digest`、`failed_publication_keeps_previous_digest`、`source_changes_during_comparison_reject_publication`；重启后同一输入也不重算。

- [ ] **Step 2: 跑红。** `cargo test -p api --test probe_retry repeated_outro_failure_does_not_recompare_season`。
- [ ] **Step 3: 实现确定性输入。** 新结构在 comparison 模块定义，字段只采用第 2.4 节输入；使用既有 SHA256 依赖。先按 ledger_id 和阶段排序，再使用长度前缀编码拼接，避免简单字符串连接碰撞。事务发布检验当前成员、来源和上下文仍等于快照；结果和摘要同事务写入。

持有短数据库锁读取快照/提交，不在锁内拉流或做昂贵匹配。输入未变直接 debug 记录 skip；输入有变但开关已关不发布、不更新成功摘要。保留 force marker_refresh 明确刷新语义，不因为自动摘要去重跳过用户强制要求。

- [ ] **Step 4: 跑绿。** `cargo test -p store`、`cargo test -p api`；执行原有 marker 原子发布和多版本冲突测试。
- [ ] **Step 5: 提交。** `perf(api): skip season comparisons for unchanged fingerprint inputs`。

### Task 5: 开关统一与自适应模式兼容

**Files:** policy、probe、worker、season、marker_resolver、HTTP library_chapters/library_config；probe_retry、既有 adaptive 集成测试。

**Consumes:** Task 2 policy、Task 3 重试状态、Task 4 安全发布。

**Produces:** 单一资格策略；关闭时无自动采集/发布；自适应流程不得绕过退避重采其他集。

- [ ] **Step 1: 写失败测试。** 覆盖两个开关四种组合；开关在 queued→running、running→publish 之间变化；开启后不提前突破原 retry deadline。

```rust
// 总开关关闭，即使 enable_fingerprint=true 也不得自动工作。
s.set_intro_settings(false, true);
s.missing_outro(8);
assert_eq!(s.request(8, ProbeRequestOrigin::Detail), ProbeRequestResult::Disabled);
assert_eq!(s.dispatch_due(), 0);
assert_eq!(s.outro_reads(8), 0);
```

support 新增 `set_intro_settings(bool,bool)`。增加 `adaptive_retry_reuses_other_episode_samples`：启用 adaptive，E08 某窗口失败，E01–E07 的有效样本不得重复采集；季协调任务仍受同一身份/开关/退避约束。为 HTTP 手动刷新断言 409 及错误码。

- [ ] **Step 2: 跑红。** `cargo test -p api --test probe_retry disabled_master_switch_blocks_fingerprint`。
- [ ] **Step 3: 实现策略。** 删除各入口重复的 enable_fingerprint 单独判断，自动和手动识别统一检查第 2.3 节资格。adaptive 模式复用既有分段样本/attempts，使用 profile/window 身份细分失败资格，不允许将一个单集补采 request 无条件升级为整季重新采集。intro/outro 状态是这些窗口结果的汇总，不建立第二份互相矛盾的窗口事实。若现有 adaptive attempt 缺 next_retry 字段，在其既有存储扩展窗口退避，沿用同一退避函数和公开资格检查。

- [ ] **Step 4: 跑绿。** `cargo test -p api --test adaptive_fingerprint_capture`、`cargo test -p api --test adaptive_fingerprint_season`、`cargo test -p api`；若此任务触及 store 同时 `cargo test -p store`。
- [ ] **Step 5: 提交。** `fix(api): honor marker detection policy across probe modes`。

### Task 6: 状态 API、前端展示与可观测性

**Files:** HTTP library/library_chapters/probe；web API 类型、新状态 helper/组件/测试、详情组件；timings 与阶段日志。

**Consumes:** Task 1–5 的阶段事实和统一 request。

**Produces:** 文件级 `probe_stages` 加法 DTO、partial 展示、手动失败阶段重试、清晰日志。

- [ ] **Step 1: 写失败测试。** HTTP 公共 seam 断言 E08 的 metadata/intro succeeded、outro failed、next_retry_at_ms 和 attempt；原字段仍兼容。status GET 不创建 Job，active=false 但 retry_waiting=true。

新增 `web/lib/probe-status.ts` 导出：

```typescript
export type ProbeStageView = {
  status: "pending" | "queued" | "running" | "succeeded" | "failed" |
    "not_applicable" | "cancelled";
  failure_count: number;
  next_retry_at_ms: number | null;
  error_kind: string | null;
};
export function probeStatusText(stage: ProbeStageView, nowMs: number): string;
export function shouldPollProbeDetails(stages: ProbeStageView[]): boolean;
```

Node 测试固定 nowMs，不使用源码字符串搜索：

```javascript
import assert from "node:assert/strict";
import test from "node:test";
import { probeStatusText, shouldPollProbeDetails } from "../lib/probe-status.ts";

test("partial failure waits without keeping detail polling alive", () => {
  const stage = {
    status: "failed", failure_count: 1,
    next_retry_at_ms: 1_060_000, error_kind: "http_403",
  };
  assert.equal(probeStatusText(stage, 1_000_000), "读取被拒绝，60 秒后重试");
  assert.equal(shouldPollProbeDetails([stage]), false);
});
```

- [ ] **Step 2: 跑红。** `cargo test -p api --test probe_retry stage_status_is_read_only`；`pnpm --dir web test`。
- [ ] **Step 3: 实现加法 DTO 和 UI。** detail.files 增加 `probe_stages`（metadata/intro/outro，可空兼容旧服务）；不把 retry_waiting 映射为 probe_queued。既有 status API 保留 job 字段语义，额外返回文件阶段、retry_waiting 和最早重试时间，不替换 forced refresh job。

新增已认证可见性校验后的 `POST /api/v1/libraries/{id}/items/{item_id}/files/{ledger_id}/probe/retry`，仅重试所选文件失败阶段，成功缓存不清除；响应统一 envelope，`result=queued|already_running|complete|disabled|cancelled`。与现有 Library 手动操作保持权限一致，跨 Library/Media ledger 返回 404，不能用 ledger_id 绕过可见性。

等待退避时停止 5 秒整条详情 reload；组件本地显示倒计时，到期只发一次轻量 status 查询，再每 30 秒刷新轻量状态，卸载清理 timer；若后台进入 queued/running 才恢复现有 5 秒详情刷新。状态 GET 绝不触发补采，失败耗尽停止倒计时并显示“重试失败阶段”。旧服务未提供 probe_stages 时保留既有 probe_queued 行为。

日志包含 job_id、ledger_id、media_id、season、episode、stage、source_version、origin、attempt、error_kind、next_retry_at_ms、cache_hit、samples_changed、comparison_digest、comparison_skipped_reason。缓存命中不得再打印“流信息探测开始”；真正调用 ffprobe 前才记录实际读取。远程诊断去除 URL token、签名及认证参数。耗时汇总区分实际读取次数、缓存命中、退避跳过、比对次数。

- [ ] **Step 4: 跑绿。** `cargo test -p api`；`pnpm --dir web test`、`pnpm --dir web typecheck`、`pnpm --dir web build`。不直接运行 npm install 或升级依赖。前端未保存改动需逐块检查，不整文件覆盖。
- [ ] **Step 5: 提交。** `feat(web): expose partial probe outcomes and bounded retries`。

### Task 7: 原场景验收、完整回归与交付

**Files:** probe_retry/support、store tests、ADR、本方案勾选状态；无额外业务扩展。

**Consumes:** 全部任务。

**Produces:** 可重复的“删 E08→超宽限→恢复→片尾失败→到期补采”自动回归及验证记录。

- [ ] **Step 1: 写完整场景测试。** 通过公开删除/扫描入口或其实际公共用例函数删除 E08 并跨过假时钟宽限；恢复相同 STRM。预置其他 7 集有效缓存，E06 保留独立失败状态以验证不被 E08 恢复重置。让 E08 首次片头成功、片尾失败；重复详情查询 20 次；重启；时钟推进到期；让片尾成功。断言：E01–E07 无新增媒体/声纹读取；E08 metadata=1、intro=1、outro=2；首次普通声纹终态 partial；未到期无新 Job；到期只 fingerprint_probe；相同输入无重复季发布；最终两端标记完整。测试使用假数据/引擎，不操作用户当前 Library。
- [ ] **Step 2: 跑精确场景。** `cargo test -p api --test probe_retry delete_restore_retries_only_missing_episode_stage -- --nocapture`；测试失败先修原任务，不放宽断言。
- [ ] **Step 3: 全量验证。**

```bash
cargo test -p store
cargo test -p api
cargo test --workspace
pnpm --dir web test
pnpm --dir web typecheck
pnpm --dir web build
git diff --check
```

执行时使用环境提供的 Node/pnpm 路径；记录命令、exit code、失败是否为本次引入。检查所有触及 Rust 文件 `wc -l`，函数不得超过 120 行。验证不要求连接远端 115、真实云盘或播放设备。

- [ ] **Step 4: 代码与方案核对。** 重点检查 partial 的全部终态消费者、认领事务、源版本提交检查、重启恢复、disabled 的手动接口、adaptive 路径和前端 timer 清理；检查未产生新的私有 scheduler 或平行缓存身份。
- [ ] **Step 5: 最终提交与交付。** 仅提交本次尚未提交的测试/文档，`test: cover incremental probe retries across deletion and restart`；交付实际变化、用户可见效果、验证结果和仍不支持的跨台账复用边界。不得宣称远端 403 根因已修复。

## 6. 验收矩阵

| 场景 | 必须满足 | 任务 |
|---|---|---|
| E08 超宽限恢复 | 仅 E08 重新读取；不清其他集有效缓存 | 2、7 |
| 媒体有效、片尾缺失 | 直接声纹 Job；不新增媒体 Job | 2 |
| 片头成功片尾失败 | partial，片头立即持久化，片尾有退避 | 1、2、3 |
| 详情反复查询 | 同一活动 Job；未到期不再排队 | 2、3 |
| 不访问详情 | 到期后台仍能重试 | 3 |
| 进程重启 | 退避/耗尽保留；声纹恢复不绕回媒体 | 1、2、3 |
| 第五次失败 | 停止自动重试；手动只补失败阶段 | 3、6 |
| 样本未变 | 不匹配、不重复发布标记/章节 | 4 |
| 片尾成功或成员变化 | 输入摘要变化，合法请求重新比对 | 4 |
| 失败发布/旧快照 | 不更新成功摘要，不覆盖新来源 | 4 |
| 关闭总开关 | 不自动采集/发布；手动 409；旧标记保留 | 5 |
| adaptive | 有效其他集样本不重采，遵守同一重试资格 | 5 |
| 已有强制整季刷新 | 失败保留旧结果，成功才原子替换 | 1、2、4、5 |
| 状态呈现 | 清楚区分缓存、部分成功、等待和耗尽 | 6 |
| 原因日志 | 403/404/EOF 分类有据，敏感 URL 不泄漏 | 3、6 |

## 7. 自查结果与执行前注意

- 规格覆盖：前五项均有任务与验收；第六项跨台账缓存复用明确排除。
- 不误诊：目录遍历和整季缓存比对不等于视频重新读取；不能用减少日志掩盖真实读取。
- 类型统一：所有时间毫秒，状态 partial 是普通 Job 终态，stage waiting 由 failed + next_retry 推导，不另造互斥字符串。
- 持久化统一：阶段状态、Job 创建认领、成功摘要都在现有 library.db；后台 tick 只派发，不拉流。
- 数据兼容：迁移幂等，旧缓存按版本懒初始化；旧 succeeded 历史不冒充完整阶段；强制批次仍原子。
- 调查开始时工作区曾有大量未提交改动；写方案后复查 `git status --short` 仅显示本方案为未跟踪文件，说明期间工作区基线已变化。执行前必须重新读取状态及相关实现，不能套用旧行号或恢复旧改动；本方案不授权清理用户改动。
- 本次方案未运行业务测试；文档中的测试代码是拟新增的回归契约，不是当前已存在的测试结果。
