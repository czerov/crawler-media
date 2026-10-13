# 声纹自适应采样优化 Implementation Plan

> **历史状态（2026-10-13 整理）：** 已实现的设计/任务历史：7584e17、6527d21、87f8d8f、5eaf6b6、93ecb0a、072d87d、b7730be 及后续 review 修复交付了独立采集、规划、持久化、季编排、进度与 benchmark 支持。真实整季 STRM 三组配对、人工边界和真实 UI/VidHub 验收仍不能由 fixture/报告测试代替；未勾选步骤不表示算法未实现。
>
> 本文保留原设计、发现/任务 ID 与当时证据；日期本身不表示过时。旧执行/提交/清理命令仅作历史记录，不应直接重跑；复选框不是当前实现清单。当前依据：[数据库/API review](<../../audits/2026-10-13-database-api-review.md>) 与 [剩余计划](<2026-10-13-database-api-followup.md>)，未覆盖的旧 ID 仍须单独对账。

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 减少同季重复片头片尾的远程读取，让每集仍有准确的直接采样证据，并提供真实整季耗时和流量验收。

**Architecture:** 保留完整采集作为基准和回退，使用三个至多五个完整建模集产生模板，再对其他集用一个覆盖候选完整区间的连续窗口验证。采集证据、模板和执行进度存入现有 SQLite；纯规划/匹配放在 marker，缓存协调和持久化 Job 编排放在 api，整季结果与章节缓存原子发布。

**Tech Stack:** Rust workspace、Tokio、SQLite/rusqlite、tracing、rusty-chromaprint 0.3、现有 FFmpeg 进程、TypeScript API DTO。

**Spec:** [2026-10-08-adaptive-fingerprint-sampling-design.md](<../specs/2026-10-08-adaptive-fingerprint-sampling-design.md>)。本计划保留原详细方案草案及当时验收目标；后续已实施，当前状态见顶部，不应重新执行整份计划。代码研究起点 `main@0fd078e`；已核对交付时新增的 `6fc3f9c` 仅调整章节场景图回退，没有把计划中的性能目标视为实测结果。

## Global Constraints

- 媒体信息是必需数据，先提取并写入 SQLite；声纹是可选的后台工作，关闭声纹或 TheIntroDB 不得阻止媒体信息提取。
- 自动处理复用未变化文件的采集证据。用户手动点击片头片尾生成，要执行新的采集和识别；已有同范围 Job 时返回“正在生成”，不得重复排队。
- 重算期间显示旧标记；整季结果和章节缓存提交成功后才展示新结果。采集、数据库或发布失败时保留旧结果。
- 单集片头片尾不能直接复制其他集的时间戳。合法片头可以从 `0:00` 开始；是否准确必须同时检查结束点和实际内容。
- 远程媒体探测、声纹采集继续直连，清除代理环境；Playback 的现有直连行为不得回退。
- 所有网络、重试、采样和比对必须有可关联的日志；“PCM 输出字节数”不得称为“远程下载字节数”。
- 采集继续为单声道、16,000Hz、s16le、`preset_test2`。初版默认 `fingerprint_sampling_mode=full_window`；只有准确性和性能验收通过后才对测试范围选择 `adaptive`。
- 冻结参数：建模集 3/最多 5，模板每类最多 3，保护范围 10,000ms，最小预计窗口节约 0.15，匹配长度 15,000–240,000ms，score 上限 4.0，参考覆盖至少 0.95，内部 gap 至多 1,000ms，参考边界差至多 1,000ms，边界证据至少 3,000ms，模板两端锚点各 15,000ms，每类最多 2 个窗口、4 个累计 attempts，每个进程 deadline 120,000ms。
- 快速采样失败后至多一个完整窗口回退；独立指纹数组不得拼接。未知流量为 null，不能用 PCM 字节数替代。
- 每个 touched crate 运行 `cargo test -p <crate>`；结束运行一次 `cargo test --workspace`。单 crate 测试使用 fake/fixture，不访问公网或启动 Chromium。
- `.rs` 文件最多 800 行，生产函数最多 120 行；修改已超限的 `store/src/library.rs` 时按相关职责拆分。
- 在 worktree 开发；提交已验证的当前任务变更。按用户既定交付方式合并、提交到 main，不 push、不使用 GitHub issue。

## Review Focus

1. 同一集有多个文件版本，不能重复计支持；边界冲突不能以一个结果覆盖全部版本。Task 3 和 Task 5 验证。
2. 读取提前 EOF、样本截止或仅中心音乐一致，不能把裁断区间当完整片头片尾。Task 1、Task 3 和 Task 4 验证。
3. 采集后文件变化、重启中断或数据库提交失败，不能发布新旧混合结果。Task 2 和 Task 5 验证。
4. 手动强制请求不能被入队函数改成缓存重分析；公共库开关和媒体信息优先必须互不干扰。Task 5 和 Task 6 验证。
5. 签名 URL 泄露、未知流量显示为零、重试漏计以及错将缓存测试作为整季性能，必须被日志和报告测试拒绝。Task 1、Task 2 和 Task 7 验证。

---

## 文件职责图

| 层 | 创建/修改文件 | 唯一主要职责 |
|---|---|---|
| marker | `src/fingerprint/capture.rs`、`capture_types.rs` | 单次可替换采集、结果/错误/计量协议 |
| marker | `src/fingerprint/chromaprint/{pcm,timing,diagnostics}.rs`、`chromaprint.rs`、`target.rs` | FFmpeg 生命周期、准确窗口 seek、计量返回和脱敏 |
| marker | `src/adaptive/{types,model,planner,verify}.rs`、`src/adaptive.rs` | 模板发现、窗口决策、完整证据验证 |
| marker | `src/matcher/consensus.rs`、`consensus/cluster.rs` | 给模板提供现有聚类证据，避免另写相似匹配算法 |
| store | `src/fingerprint_samples.rs`、`fingerprint_models.rs`、`fingerprint_attempts.rs` | 样本、模型成员和每次执行的数据库接口 |
| store | `src/schema/fingerprint.rs`、`src/schema.rs` | 增量迁移；新 DDL 不堆入已 758 行的 schema.rs |
| store | `src/library/markers.rs`、`src/library.rs` | 将标记结果事务从 819 行文件中提取，保留原 public re-export |
| api | `src/fingerprint_job/adaptive/{types,cache,capture,analysis}.rs`、`adaptive.rs` | 单集自适应采集、缓存协调和输入输出转换 |
| api | `src/probe_manager/{season,recovery,progress}.rs` | 季计划、恢复、持久阶段和汇总；避免 probe_manager.rs 超限 |
| api | `src/probe_manager/{probe,worker,marker_jobs,markers,timings}.rs` | 接入新协调器、优先等待和原子发布 |
| api/web | `src/http/library_chapters.rs`、`src/http/library/probe.rs`、`web/lib/api/libraries.ts` | 保持刷新/状态协议兼容，新增可选状态 DTO |
| harness | `crates/api/examples/fingerprint-season-bench.rs`、`scripts/fingerprint-bench/*` | 可显式运行的隔离真实/受控实验与报告 |

不要求每个新增文件达到软行数上限；文件只在其 task 需要时创建。不是先铺空模块再开发。

## Task 1: 可返回的单次采集证据、时间覆盖与计量

**Files:**
- Create: `crates/marker/src/fingerprint/capture.rs`, `capture_types.rs`
- Modify: `crates/marker/src/fingerprint.rs`, `crates/marker/src/lib.rs`, `crates/marker/src/target.rs`
- Modify: `crates/marker/src/fingerprint/chromaprint.rs`, `chromaprint/pcm.rs`, `chromaprint/timing.rs`, `chromaprint/diagnostics.rs`
- Test: `crates/marker/tests/capture_evidence.rs`, `capture_diagnostics.rs`, existing `direct_media_requests.rs`

**Interfaces:**
- Consumes: 现有 `ProbeTarget`、`FingerprintEngine` 和 `Configuration::preset_test2()`；保留现有 `extract_at` 和 matcher 调用兼容。
- Produces: 新公开 `FingerprintCaptureEngine`，一次调用只启动一次采集；重试由 Task 4 的总预算统一控制。

```rust
pub struct SampleWindow { pub start_ms: i64, pub end_ms: i64 }
pub struct CaptureRequest {
    pub path: std::path::PathBuf,
    pub window: SampleWindow,
    pub audio_stream_index: Option<u32>,
    pub process_deadline_ms: u64,
}
pub enum InputBytesSource { AvioInput, FixtureOrigin }
pub struct CaptureMetrics {
    pub elapsed_ms: u64,
    pub time_to_first_pcm_ms: Option<u64>,
    pub pcm_read_wait_us: u64,
    pub chromaprint_consume_us: u64,
    pub pcm_bytes: u64,
    pub input_bytes: Option<u64>,
    pub input_bytes_source: Option<InputBytesSource>,
    pub measurement_complete: bool,
    pub ffmpeg_exit_code: Option<i32>,
}
pub struct CapturedFingerprint {
    pub window: SampleWindow,
    pub words: Vec<u32>,
    pub pcm_duration_ms: Option<i64>,
    pub metrics: CaptureMetrics,
}
pub enum CaptureFailureKind { Io, Timeout, EmptyAudio, InvalidWindow, Decode }
pub struct CaptureFailure {
    pub kind: CaptureFailureKind,
    pub message: String,
    pub metrics: CaptureMetrics,
}
pub trait FingerprintCaptureEngine: Send + Sync {
    fn capture_window(&self, request: &CaptureRequest)
        -> Result<CapturedFingerprint, CaptureFailure>;
}
```

`CaptureFailure` 实现 thiserror；新增协议可 serde 往返。保留已有 benchmark/CPU 字段作为 `CaptureMetrics` 的具名扩展字段，不因列举基础字段而删除诊断。

- [ ] **Step 1: 写失败测试**：`captured_window_reports_actual_pcm_coverage` 断言请求 `[100_000,140_000]`、实际只出 12 秒 PCM 时返回实际 12,000ms，不能声称覆盖 40 秒；`unknown_input_bytes_remain_null` 断言 `input_bytes=None`，即使 PCM 字节大于零；`diagnostics_redact_signed_urls_and_auth_headers` 使用现有脱敏案例；`sample_request_keeps_proxy_disabled` 断言实际 outbound command 的代理设置和 input seek 顺序。

  两个独立测试的关键断言：
  ```rust
  assert_eq!(capture.window.start_ms, 100_000);
  assert_eq!(capture.window.end_ms, 140_000);
  assert_eq!(capture.pcm_duration_ms, Some(12_000));
  assert!(capture.metrics.pcm_bytes > 0);
  assert_eq!(capture.metrics.input_bytes, None);
  ```

- [ ] **Step 2: 跑红**：`cargo test -p marker --test capture_evidence` 和 `cargo test -p marker --test capture_diagnostics`。预期缺少新协议或覆盖/计量断言失败。
- [ ] **Step 3: 实现协议和单次采集**：`SampleWindow::validate() -> Result<(), CaptureFailure>` 禁止负时间或空区间；`ProbeTarget::apply_ffmpeg_input_with_seek_ms(&self, command: &mut Command, start_ms: i64)` 使用十进制秒；保留旧整数秒方法作兼容包装。`ChromaprintEngine` 实现新 trait，PCM duration 根据真实 sample_count 算；deadline 到期 kill + wait，返回 Timeout 和已有度量。保留默认 accurate seek。选中音轨以已保存索引显式 map；无索引保留原自动选择并禁用需要精确音轨 profile 的快速路径。
- [ ] **Step 4: 验证**：运行上述两套新测试、现有 direct_media_requests 和 `cargo test -p marker`；长 GOP、非零 PTS 容器的实际 FFmpeg 校准留给 Task 7 opt-in harness，不让纯测试依赖外部进程。
- [ ] **Step 5: 提交**：只 stage 本任务文件；commit `feat(marker): return capture coverage and input diagnostics`。

## Task 2: 按连续窗口保存样本、模型成员和 attempts

**Files:**
- Create: `crates/store/src/fingerprint_samples.rs`, `fingerprint_models.rs`, `fingerprint_attempts.rs`, `schema/fingerprint.rs`
- Create: `crates/store/src/library/markers.rs`
- Modify: `crates/store/src/{lib,schema,library,fingerprint_cache,probe_tasks}.rs`
- Test: `crates/store/tests/fingerprint_samples.rs`, `fingerprint_attempts.rs`, `fingerprint_cache.rs`

**Interfaces:**
- Consumes: Task 1 的字段协议；Store 不直接依赖 marker，API 负责把 Store DTO 与 marker 类型转换。
- Produces: `StoredFingerprintSample`、`StoredFingerprintModel`、`StoredFingerprintModelMember`、`StoredFingerprintAttempt`、`FingerprintSampleQuery`。字段与设计第 8 节表定义逐项同名；窗口时间为 i64 ms，score/coverage 留在版本化模型和 outcome JSON。

```rust
impl Store {
    pub fn find_covering_fingerprint_samples(&self, query: &FingerprintSampleQuery)
        -> Result<Vec<StoredFingerprintSample>, StoreError>;
    pub fn begin_fingerprint_attempt(&self, attempt: &StoredFingerprintAttempt)
        -> Result<(), StoreError>;
    pub fn complete_fingerprint_attempt(&self, attempt: &StoredFingerprintAttempt,
        sample: Option<&StoredFingerprintSample>) -> Result<(), StoreError>;
    pub fn put_fingerprint_model(&self, model: &StoredFingerprintModel,
        members: &[StoredFingerprintModelMember]) -> Result<(), StoreError>;
    pub fn list_fingerprint_models(&self, media_id: &str, season: u32)
        -> Result<Vec<StoredFingerprintModel>, StoreError>;
    pub fn put_probe_sampling_plan(&self, job_id: &str, ledger_id: &str,
        plan_json: &str) -> Result<(), StoreError>;
    pub fn put_probe_detection_outcome(&self, job_id: &str, ledger_id: &str,
        outcome_json: &str) -> Result<(), StoreError>;
}
```

`FingerprintSampleQuery` 含 `ledger_id/source_version/capture_profile_key/kind/window_start_ms/window_end_ms/captured_job_id`；最后一项 `None` 为自动可复用，`Some(job)` 为 Recapture 恢复时仅查询本次 Job。`StoredFingerprintAttempt` 状态限定 running/succeeded/failed/interrupted。

- [ ] **Step 1: 写失败测试**：`samples_survive_reopen_and_preserve_absolute_windows` 验证落盘并 reopen；`covering_sample_requires_same_source_profile_kind_and_job` 对不同版本/音轨/profile 和 Recapture 的旧 Job 样本断言不匹配；`attempt_and_sample_commit_together` 注入事务失败后两者都不宣称成功；`deleting_file_metadata_invalidates_samples_and_referencing_models`；`interrupted_attempt_preserves_unknown_byte_measurement` 验证恢复后不是 0。

  reopened 使用相同 source/profile、窗口 `[100_000,140_000]` 的查询；changed_source_matches 使用不同 source 查询：
  ```rust
  assert_eq!(reopened.len(), 1);
  assert_eq!(reopened[0].window_start_ms, 100_000);
  assert_eq!(reopened[0].window_end_ms, 140_000);
  assert!(changed_source_matches.is_empty());
  ```

- [ ] **Step 2: 跑红**：`cargo test -p store --test fingerprint_samples` 和 `cargo test -p store --test fingerprint_attempts`，预期缺 API/表或行为失败。
- [ ] **Step 3: 实现增量迁移和 DTO**：新 DDL 写在 schema/fingerprint.rs，schema.rs 调用；为 kind/window/profile 唯一键和 model_members 外键加约束。模型与成员同事务写；删除样本前删除引用模型。新 Job 字段按设计第 8 节迁移，旧记录 sampling mode 解释为 full_window、媒体信息复用字段有明确兼容默认。把 marker transaction 和其相关测试提取至 library/markers.rs，原类型 re-export 保持不变。
- [ ] **Step 4: 验证**：运行新 tests、旧 fingerprint_cache、旧 marker 原子提交测试和 `cargo test -p store`；迁移运行两次、旧数据库 reopen 无重复或丢失。`wc -l crates/store/src/library.rs crates/store/src/schema.rs` 均不超过 800。
- [ ] **Step 5: 提交**：commit `feat(store): persist fingerprint windows models and attempts`。

## Task 3: 纯模板发现、窗口规划和完整区间验证

**Files:**
- Create: `crates/marker/src/adaptive.rs`, `adaptive/types.rs`, `adaptive/model.rs`, `adaptive/planner.rs`, `adaptive/verify.rs`
- Modify: `crates/marker/src/lib.rs`, `matcher/consensus.rs`, `matcher/consensus/cluster.rs`
- Test: `crates/marker/tests/adaptive_templates.rs`, `adaptive_windows.rs`, `adaptive_verification.rs`

**Interfaces:**
- Consumes: Task 1 `SampleWindow/CapturedFingerprint`、既有 `FingerprintEngine::find_common_segments`。
- Produces: `SegmentKind { Intro, Outro }`；`SamplingMode { FullWindow, Adaptive }`；`SamplingPolicy` 使用 Global Constraints 参数；`EpisodeEvidence` 保存 sample_id、ledger_id、episode、source_version、capture_profile_key、kind、capture；`EpisodeDescriptor` 保存 ledger_id、episode、source_version、duration_ms。
- Produces: `TemplateModel` 保存 model_id/version/kind、每个 `TemplateReference` 的 sample_id/ledger_id/episode/source_version/绝对匹配区间、稳定状态；`TemplateContext { models, references }`；`VerifiedInterval { start_ms, end_ms, supporting_episodes, score, coverage }`。`WindowDecision` 的 Full 带 window/reason，Verify 带 window/model_ids；`VerificationOutcome` 的 Verified 带 VerifiedInterval，NeedsFullWindow/NoMatch/SamplingLimit 带具名 reason，使用设计中的明确 reason code。

```rust
pub fn select_seed_episodes(episodes: &[EpisodeDescriptor], policy: &SamplingPolicy)
    -> Vec<String>; // ordered ledger IDs; unique episode numbers
pub fn build_season_models(engine: &dyn FingerprintEngine,
    evidence: &[EpisodeEvidence], policy: &SamplingPolicy) -> Vec<TemplateModel>;
pub fn plan_episode_window(episode: &EpisodeDescriptor, kind: SegmentKind,
    templates: &TemplateContext, policy: &SamplingPolicy,
    cost: &SourceCostSummary) -> WindowDecision;
pub fn verify_template_window(engine: &dyn FingerprintEngine,
    target: &EpisodeEvidence, templates: &TemplateContext,
    policy: &SamplingPolicy) -> VerificationOutcome;
```

`SourceCostSummary` 字段为 `median_first_pcm_ms: Option<u64>`、`sample_ms_per_wall_ms: Option<f64>`、`measured_samples: u32`、`recent_fast_attempts: u8`、`recent_fallbacks: u8`；按 `(job_id, origin_key, kind)` 统计，origin key 只 hash scheme/host/port。至少三次有效度量时采用设计第 7.2 节 H/R/P 成本式，要求预计成本改善至少 10%；没有测量时只用窗口 15% 门槛。`TemplateReference` 不复制指纹字节，references 通过 sample_id 提供原始连续样本。

- [ ] **Step 1: 写失败测试**：固定 8 集选择 E02/E04/E07；`same_episode_versions_count_once_as_support`；`intro_zero_start_is_valid_with_other_boundary_evidence`；`outro_prediction_uses_target_duration` 使用不同总时长但相同距末尾位置；`multiple_credit_variants_remain_separate`；`center_only_match_requires_full_window`；`window_edge_match_is_not_a_complete_template`；`edited_middle_and_conflicting_references_require_fallback`；`constant_fingerprints_do_not_form_trusted_templates`；`close_candidates_share_one_contiguous_window`；`large_prediction_skips_fast_path`。

  中心 20 秒相同、模板 90 秒时，调用 verify_template_window 的关键断言：
  ```rust
  assert!(matches!(outcome,
      VerificationOutcome::NeedsFullWindow { .. }));
  // 合法从 0 开始的另一个 fixture：
  assert_eq!(verified.start_ms, 0);
  assert_eq!(verified.end_ms, 90_000);
  ```

- [ ] **Step 2: 跑红**：分别运行三个新 integration test target，预期缺模块/函数或候选完整性断言失败。
- [ ] **Step 3: 实现算法**：复用 existing match candidates 和 cluster membership，保留独立 episode 支持规则；阶段性的两成员候选不能启用快速路径。按设计第 7 节包络/保护/clip 规则规划；所有候选用一次连续窗口验证，两独立 reference 的边界差不超过 1,000ms，coverage 至少 0.95，score 至多 4.0，模板两端各 15,000ms 锚点都被覆盖。Item 时间来自 Configuration；模板边界用于预测，输出从目标匹配坐标计算。常量/沉默证据不得单独达成有效模板；阈值效果由 Task 7 校准，不以任意音量代替相似性。
- [ ] **Step 4: 验证**：新 tests 加 existing `season_consensus`, `multiple_candidates`, `fingerprint_accuracy` 和 `cargo test -p marker`；全窗口旧正确案例不因模板接口改动退化。
- [ ] **Step 5: 提交**：commit `feat(marker): plan and verify season template windows`。

## Task 4: 单集缓存协调、总重试预算和安全回退

**Files:**
- Create: `crates/api/src/fingerprint_job/adaptive.rs`, `adaptive/types.rs`, `adaptive/cache.rs`, `adaptive/capture.rs`, `adaptive/analysis.rs`
- Modify: `crates/api/src/fingerprint_job.rs`, `fingerprint_job/cache_key.rs`, `fingerprint_job/cache.rs`
- Modify: `crates/api/src/scrape_config.rs`, `crates/api/src/http/scrape_settings.rs`
- Test: `crates/api/tests/adaptive_fingerprint_capture.rs`, `fingerprint_cache.rs`

**Interfaces:**
- Consumes: Tasks 1–3，Store DTO 显式转换，profile 不依赖 analysis policy。
- Produces: `CapturePolicy { ReuseValid, Recapture }`；`EpisodeCaptureRequest` 含 job_id、domain::LedgerRow、source_version、media_duration_ms、已选音轨、capture_profile_key、policy、capture_policy、TemplateContext。`EpisodeDetection` 保存 intro/outro 的 detected/no_match/insufficient_peers/sampling_limit/capture_failed 输出及证据引用；错误不能丢失 attempts。

```rust
pub struct AdaptiveCaptureContext {
    pub store: std::sync::Arc<parking_lot::Mutex<Store>>,
    pub matcher: std::sync::Arc<dyn marker::FingerprintEngine>,
    pub capture: std::sync::Arc<dyn marker::FingerprintCaptureEngine>,
    pub gate: std::sync::Arc<dyn CaptureGate>,
}
pub trait CaptureGate: Send + Sync {
    fn wait_before_capture(&self)
        -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>>;
}
pub async fn capture_episode_adaptive(ctx: &AdaptiveCaptureContext,
    request: &EpisodeCaptureRequest) -> Result<EpisodeDetection, CaptureEpisodeError>;
pub fn capture_profile_key(profile: &FingerprintCaptureProfile) -> String;
pub fn analysis_policy_key(policy: &marker::SamplingPolicy) -> String;
```

`FingerprintCaptureProfile` 字段为 `algorithm_version: u32`、`preset: String`、`pcm_sample_rate: u32`（16,000）、`pcm_channels: u16`（1）、`pcm_format: String`（s16le）、`audio_stream_index: Option<u32>`、`audio_selection_version: u32`、`time_mapping_version: u32`；两个 version 的首版值为 1。`CaptureEpisodeError` 使用 thiserror 分类并携带持久 attempt IDs。Stage 等待只发生在新采集前，不中止正在进行的 FFmpeg。

- [ ] **Step 1: 写失败测试**：`reuse_valid_does_not_read_media` 从重新打开的临时 Store 获得相同区间；`recapture_requires_new_job_evidence` 即使旧样本存在也保存本次新 attempt；`recapture_restart_reuses_only_own_completed_samples`；`successful_short_window_avoids_full_capture` 断言持久实际 windows；`uncertain_window_falls_back_once`；`partial_pcm_never_becomes_detected`；`retries_and_fallback_share_four_attempt_budget`；`policy_change_reuses_capture_evidence`；`changed_audio_stream_invalidates_only_affected_evidence`。

  重试耗尽 fixture 中读取实际持久 attempts，而不是只统计 fake 调用：
  ```rust
  assert_eq!(attempts.len(), 4);
  assert!(attempts.iter().all(|attempt| attempt.status == "failed"));
  assert!(new_successful_samples.is_empty());
  ```

- [ ] **Step 2: 跑红**：`cargo test -p api --test adaptive_fingerprint_capture`。预期缺接口或新证据/预算/复用断言失败。
- [ ] **Step 3: 实现协调器**：按 capture policy 查询同来源/profile/覆盖缓存；旧完整缓存只通过已验证 adapter 使用。每次真实尝试前写 running record，在 spawn_blocking 中调用一次 capture trait，结束持久计量；可恢复 IO 错误用现有 500/1,000/2,000ms 等待但总预算只到 4。成功但验证不确定才消耗一次完整回退，不因纯 IO 错误盲目改变窗口；deadline 和无音频为明确终态。profile 与 analysis keys 分离。新增 sampling mode 设置严格接受 `full_window|adaptive`，缺省 full_window，不新增前端高级参数表单。
- [ ] **Step 4: 验证**：运行新 test、现有 fingerprint_cache/fingerprint_job 和 `cargo test -p api`。短样本不能写入旧表冒充 180 秒完整缓存；score 改动不改变新 capture key。
- [ ] **Step 5: 提交**：commit `feat(api): capture episode windows with bounded fallback`。

## Task 5: 持久化季编排、强制语义和原子发布

**Files:**
- Create: `crates/api/src/probe_manager/season.rs`, `recovery.rs`, `progress.rs`
- Modify: `crates/api/src/probe_manager.rs`, `probe_manager/{probe,worker,queue,marker_jobs,markers,timings}.rs`
- Modify: `crates/store/src/probe_tasks.rs`, `crates/store/src/library/markers.rs`
- Test: `crates/api/src/probe_manager/marker_refresh_tests.rs`, `queue_tests.rs`
- Test: `crates/api/tests/adaptive_fingerprint_season.rs`, `metadata_priority.rs`

**Interfaces:**
- Consumes: Task 4 collector + Task 2 持久计划/结果。
- Produces: `ProbeManager::with_engines(store: Arc<Mutex<Store>>, matcher: Arc<dyn FingerprintEngine>, capture: Arc<dyn FingerprintCaptureEngine>) -> Self`，保留已有构造器兼容；`ProbeUnit` 增加独立 `reuse_media_info_cache: bool`，已有 reuse fingerprint flag 只表达采集复用，不再同时控制媒体信息。
- Produces: `SeasonSamplingPlan` 保存 job_id、media_id、season、sampling_mode、analysis_policy_key、ledger/source 快照、建模顺序、完成 evidence/outcome 引用；`MetadataPriorityGate` 实现 CaptureGate；阶段和统计以 SQLite 记录为真值。

- [ ] **Step 1: 写失败测试**：`season_refresh_does_not_overwrite_recapture_policy` 从实际 enqueue_marker_refresh 后读数据库断言 `reuse_fingerprint_cache=false`、`reuse_media_info_cache=true`；`metadata_refresh_still_bypasses_metadata_cache`；`seed_units_complete_before_template_verifications` 观察持久 sampling_plan 和窗口结果；`new_metadata_waits_only_for_current_sample` 用受控 gate 验证样本完成后新读取等待；`disabled_fingerprint_and_introdb_still_persist_media_info`；`resume_season_keeps_successful_evidence_and_old_visible_markers`；`source_changes_before_publish_keep_old_results`；`two_versions_with_different_boundaries_fail_without_overwrite`；`database_failure_rolls_back_markers_chapters_and_job_terminal`。

  强制请求实际入队后，按持久 unit 检查政策，并在注入发布失败的另一 fixture 检查 old-result：
  ```rust
  assert!(!persisted_unit.reuse_fingerprint_cache);
  assert!(persisted_unit.reuse_media_info_cache);
  assert_eq!(visible_marker.intro_end_ms, Some(104_000));
  assert_eq!(job.status, "failed");
  ```

- [ ] **Step 2: 跑红**：`cargo test -p api --test adaptive_fingerprint_season`、`cargo test -p api --test metadata_priority` 和 `cargo test -p api season_refresh_does_not_overwrite_recapture_policy`。最后一项须针对现有 true 覆盖找到真实回归，不只测试 DTO 默认值。
- [ ] **Step 3: 实现季计划与恢复**：强制批次持久化 Recapture；有效媒体信息可复用，显式媒体信息动作设置独立 refresh flag。批次先处理建模集，两个类别分别建立模型，再逐集执行 collector；自动单集 Job 从已有有效证据渐进建模，不预先强制采齐全季。每类最近五次至少三次回退则关闭该 Job 后续快速路径。新样本和逐集输出持久化后再 finish unit；进程重启只重建未完成计划，同 Job 成功样本可以复用。任何致命发布错误必须使 Job 达终态，不能留下永远 running 的活动范围。
- [ ] **Step 4: 完成原子发布**：在已有 complete_marker_refresh 事务里复核所有结果的来源/ledger 快照和独立 episode 支持；保留 locked。采集失败、sampling_limit、版本冲突不提交替换；真正完整范围无匹配可清除旧自动结果。标记、章节缓存、检测 provenance 和 Job 终态同事务，避免查询看到不一致。自动任务只更新有本次直接证据的集，不复制缺失集。
- [ ] **Step 5: 验证**：运行新测试、现有 queue/marker persist/refresh tests、`cargo test -p api`、`cargo test -p store`；检查 touched Rust 文件函数大小。
- [ ] **Step 6: 提交**：commit `fix(api): persist adaptive seasons and enforce forced recapture`。

## Task 6: HTTP 行为兼容与可解释的状态

**Files:**
- Modify: `crates/api/src/http/library_chapters.rs`, `crates/api/src/http/library/probe.rs`
- Modify: `web/lib/api/libraries.ts`
- Test: `crates/api/tests/management/probe_refresh.rs`
- Test: `crates/api/tests/management/adaptive_marker_status.rs`，通过已有 management test runner 注册

**Interfaces:**
- Consumes: Task 5 的持久 Job、plan/outcome/attempt records，现有 API envelope。
- Produces: 保留已有刷新返回字段；`ProbeJobStatus` 增加可选 `phase`、`sampling_mode`、`queue_wait_ms`、`priority_wait_ms`、`metrics`，phase 使用设计第 8 节阶段；metrics 的 input_bytes 为 `number|null`，并有 measurement_complete。
- Routes: `POST /api/v1/libraries/{id}/items/{item_id}/chapters/refresh` 和 `GET .../probe-status`；无需新 generation endpoint。

- [ ] **Step 1: 写失败 HTTP 测试**：`manual_refresh_with_existing_cache_queues_recapture`；`concurrent_manual_refresh_returns_same_active_job`；`polling_status_does_not_start_new_capture`；`chapter_reads_keep_old_result_until_successful_commit`；`disabled_automatic_fingerprint_does_not_hide_explicit_manual_generation`；`introdb_disabled_does_not_disable_local_capture_or_media_info`；`status_reports_partial_measurement_as_unknown`。用 fake engine 和临时 SQLite，断言返回 JSON、持久 Job 和实际章节结果。

  两次刷新都读取 API envelope.data，不以页面字符串代替状态断言：
  ```rust
  assert_eq!(first["data"]["fingerprint_refresh_job"]["id"],
      second["data"]["fingerprint_refresh_job"]["id"]);
  assert_eq!(second["data"]["fingerprint_refresh_already_running"], true);
  assert!(status["data"]["job"]["metrics"]["input_bytes"].is_null());
  ```

- [ ] **Step 2: 跑红**：`cargo test -p api --test management adaptive_marker_status`。新文件须与该 repo 的 management module 引入方式一致，不能误写一个未被执行的测试文件。
- [ ] **Step 3: 实现协议映射**：enqueue 只创建一次活动范围，成功创建持久 Job 后响应；重复返回相同 job id 和 already_running。GET 仅查询 SQLite，不依赖内存 seen 决定是否活动，不新增采集副作用。新 DTO 字段为 optional，保留现有页面的 old-marker/polling 行为；后端不输出生产 token/URL。仅补 TypeScript 类型，不创建新页面或实现日志配置 UI。
- [ ] **Step 4: 验证**：`cargo test -p api --test management`、`cargo test -p api`；在 web 目录运行 `npm run typecheck`。public static chapter images 和 authenticated Playback 路由不变，使用既有协议测试验证。
- [ ] **Step 5: 提交**：commit `feat(api): expose durable adaptive marker progress`。

## Task 7: 真实字节计量、整季基准与上线门槛

**Files:**
- Create: `crates/api/examples/fingerprint-season-bench.rs`
- Create: `scripts/fingerprint-bench/{origin.py,run.py,report.py}`
- Create: `crates/marker/tests/fixtures/adaptive/manifest.json`、相应合法合成信号参数/fixture
- Create: `docs/benchmarks/2026-10-08-fingerprint-sampling-protocol.md`
- Test: `crates/api/tests/adaptive_benchmark_report.rs`, `scripts/fingerprint-bench/test_report.py`
- Modify: `crates/marker/src/fingerprint/chromaprint/diagnostics.rs`（只有经 origin 计量确认正确时接纳 AVIO 输入字节解析）

**Interfaces:**
- Consumes: Task 4 collector/Task 5 编排，强制和缓存复用政策不得混淆。
- Produces: 每次 run 的 JSON/CSV，字段固定为 `baseline_commit, implementation_commit, run_id, sampling_mode, capture_policy, source_versions, queue_wait_ms, priority_wait_ms, total_elapsed_ms, stage_totals, attempts, input_bytes, measurement_complete, episode_intervals, ground_truth, accuracy_errors`。
- Produces: 显式命令，须实现后才存在：

```bash
cargo run -p api --example fingerprint-season-bench -- \
  --manifest <manifest.json> --sampling-mode full_window \
  --capture-policy recapture --work-dir <isolated-run-dir> --output <run.json>
python3 scripts/fingerprint-bench/run.py \
  --manifest <manifest.json> --pairs 3 --output-dir <isolated-results-dir>
python3 scripts/fingerprint-bench/report.py \
  --results <isolated-results-dir> --output <report.md>
```

Manifest 仅保存媒体本地/STRM 路径、season/episode、人工区间和确定性标注；报告只存来源 hash，不输出 STRM URL。Runner 不默认连接生产 API、清库、重启或修改正在运行的实例；源路径只读，临时数据库独立。

- [ ] **Step 1: 写失败测试**：`unknown_network_bytes_do_not_produce_saving_percent`；`recapture_report_rejects_old_cache_as_fresh_capture`；`paired_report_includes_failures_retries_and_ground_truth_errors`；`pcm_metrics_cannot_be_labeled_network_bytes`；`overlapping_stage_times_are_not_added_to_total_wall_time`。固定 run JSON fixture，断言报告标题、数值和数据质量标记。

  报告输出 JSON 的关键断言（Python report 对外函数命名 `build_report(runs: list[dict]) -> dict`）：
  ```python
  assert report["input_bytes_saving_percent"] is None
  assert report["measurement_complete"] is False
  assert report["failed_attempts"] == 2
  assert report["max_boundary_error_ms"] == 3000
  assert report["accuracy_gate_passed"] is False
  ```

- [ ] **Step 2: 跑红**：`cargo test -p api --test adaptive_benchmark_report`、`python3 -m unittest discover -s scripts/fingerprint-bench -p 'test_*.py'`。
- [ ] **Step 3: 实现 opt-in harness**：origin 支持正确的 Range、200/206、实际发送 body 字节和可控重试；合成容器含已知 90 秒片头/120 秒片尾及位置偏移、多个版本、非零 PTS、长 GOP、截断。用 `which ffmpeg` 检查能力，缺少时明确报告不可运行，不安装依赖。只有 origin 比对验证过的单一 AVIO 输入可以被当作 input_bytes，未知保持 null。
- [ ] **Step 4: 运行受控完整链路**：8 集重复 fixture 和变化多 fixture，各模式三组配对；first-run、Recapture、ReuseValid 和单集来源变化分开报告。比较真实 body 字节、每集实际 windows、开流到首 PCM、CPU 和整季 wall time。缓存 pair-matcher 合成测试另外附录展示，不能混入采集提速结果。
- [ ] **Step 5: 运行真实 STRM 和人工边界校准**：用《乩身 (2026)》和《一瓯春 (2026)》只读源路径构建 manifest；先观看所有参测集的开始/结束范围，人工标注并区分不确定项，再运行三组交错配对。检查每个已确认边界误差 ≤2 秒、新假阳性为 0、旧正确识别不丢失。没有可靠输入字节时只能报告时间和窗口覆盖改善。
- [ ] **Step 6: 评估启用条件**：8 集重复 fixture 冷采窗口覆盖减少 ≥15%，median wall time 改善 ≥10%，可靠 body 字节减少 ≥15%；变化多 fixture 每类最多两个窗口/四 attempts，median 回退 ≤10%。失败时调整成本策略/匹配校准并重测；达不到则维持 full_window，报告具体原因，不能把未通过改写为“已完成优化”。真实源不保证每季都加速。
- [ ] **Step 7: 验证**：新报告 tests、每个 touched crate 的 `cargo test -p`、web typecheck，最终一次 `cargo test --workspace`。人工和真实实验与纯测试结果分开记录。
- [ ] **Step 8: 提交**：commit `test: benchmark adaptive fingerprints across complete seasons`。只有本文和设计都经审阅且上线门槛达标，才按用户要求合并到 main；不 push。

## 任务依赖与执行方法

`Task 1 → Task 2/3 → Task 4 → Task 5 → Task 6/7`。Task 2 和 Task 3 可由独立 implementer 在不同文件中推进，但消费 Task 1 同一协议；整季编排和集成由一个负责人负责。每个任务只提交自己的已验证文件。

推荐 native 执行：纯算法、缓存和 Job 接口依赖较紧，先按顺序实现更容易维持一致语义；独立 review 在完整分支结束进行。用户若选择 subagent-driven，需按 Superpowers 对每个任务的实现与审阅流程运行，不能只把模块分完就宣布完成。

代码执行时先检查 main 与 attached worktrees，使用 Superpowers using-git-worktrees 创建或复用隔离 worktree。文档里列出的未来测试命令不是本次已经运行的测试。

## 方案自检与需求覆盖

| 设计要求 | 实施任务 |
|---|---|
| 强制重采、缓存政策和媒体信息独立 | 4、5、6 |
| 少数集发现、多版本模板、支持集数 | 3、5 |
| 完整候选验证、正确绝对坐标、EOF/截断 | 1、3、4、7 |
| 缓存版本拆分、旧缓存兼容与清理 | 2、4 |
| 重试预算、deadline、metadata priority | 1、4、5 |
| 持久进度、恢复、去重和原子发布 | 2、5、6 |
| 日志、真实输入字节、不完整统计 | 1、2、5、7 |
| 同集不同版本不计多票且冲突不乱发布 | 3、5 |
| 真正整季性能、人工真值、保守启用 | 7 |

方案审阅时重点确认“手动重采”语义和每类两窗口的成本限制。执行前若代码已超过上述基线，先核对这些接口与真实实现，再更新计划；不能照着旧行号机械修改。

## 本次方案交付检查（实际已执行）

- 已自检设计覆盖、任务依赖、接口类型一致性和五项 Review Focus；未来行为测试的关键断言随任务列出。
- 已检查两份文档的相对链接、代码围栏、尾部空格、未完成占位符和任务顺序。
- 在 `main@6fc3f9c` 的产品代码基线上运行 `cargo test --workspace`：1,017 passed、0 failed、1 ignored，进程退出码 0。
- 原始方案交付当时只创建文档；上述工作区测试验证当时基线，不证明新算法、强制重采修复或真实 STRM 提速。当时 Task 1–7 尚未执行；后续实现提交见顶部，真实 STRM 提速仍需实测证据。
