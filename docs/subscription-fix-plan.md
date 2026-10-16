# 订阅与下载修复开发计划

- 依据：`subscription-review.md`（33 条，订阅 20 + 下载 13）与技术复核 `subscription-review-tech.md`（严重度调整 + 新增 8 条）。
- 代码基线：`main` @ `d6ae414`（与审查提交 `746cac6` 的 `crates/` 目录无差异）。
- 严重度已按技术复核调整：M8 由中升为高；M4 拆为 M4a（中）/M4b（低）；L6 的跨实例部分独立为 G-M5（中）。
- 条目总数：42；按优先级：P0 4 条、P1 6 条、P2 8 条、P3 24 条；按严重度：严重 2、高 10、中 18、低 12。

## 排期与原则

| 优先级 | 目标 | 条目 |
|---|---|---|
| P0 | 止血：消除数据丢失与在途任务误删 | D-S1、S1、D-H3、M8 |
| P1 | 闭环：占位/去重/客户端稳定性 | H1、H2、H3、H4、D-H1、D-H2 |
| P2 | 正确性：选剧、准入、事务与多实例 | G-H1、G-H2、M1、M2、M3、M6、G-M2、G-M5 |
| P3 | 其余中低项、设计取舍与体验 | M4a、M4b、M5、M7、M9、D-M1、D-M2、D-M3、D-M4、D-M5、G-M1、G-M3、G-M4、L1、L2、L3、L4、L5、L6、D-L1、D-L2、D-L3、D-L4、G-L1 |

通用要求：
- 每个条目单独成一个 PR（D-H3 与 M8 可合并为一个 PR），PR 描述引用条目编号；提交前 `cargo test --workspace` 全部通过。
- 依赖关系：H3 依赖 D-S1；H1 依赖 H4 抽出的共享占位函数；G-M2 依赖 H2 的 rejected 状态；D-M3 与 D-H2 共用 hash 落库；L4/G-L1 依赖 M4a；L6 随 M6/H3 一并完成。
- 涉及 schema 的条目（H2 新 state 值、M6 待删除表、G-M5 租约表、M4a 新状态）只做新增、不做破坏性变更，以便回滚。
- 行为变化较大的条目都配有开关，默认值保持兼容。


## 开发与测试工作流（强制）

所有修复在**同一分支** `docs/subscription-fix-plan`（或由此切出的唯一修复分支，如 `fix/subscription-download`）上**按 P0→P1→P2→P3 顺序串行**完成：

1. 同一时间只做**一个**条目（D-H3 与 M8 允许合并为一个提交/PR）。
2. 先实现改法，再补齐本条的「流程测试场景」对应测试（见下节 E2E 环境），再补必要的单测。
3. 本条目相关测试与 `cargo test --workspace` 全部通过后，再开下一条；中途不得并行开多个条目。
4. 每个条目一个独立 commit（消息含编号，如 `fix(D-S1): ...`），便于 revert；合并时可用 squash，但本地过程必须可逐步回退。
5. **单测不足以验收**：涉及选种/下载/入库/洗版的条目必须以端到端流程测试为主（创建订阅 → 搜索 → 提交下载器 → 下载完成 → 入库 / 洗版替换），用 mock 下载器、索引站、TMDB、ffprobe 覆盖错误分支。
6. 合入前在本机或 CI 跑：`cargo test -p subscribe -p downloader -p api --test '*'`，以及新增的 `subscription_e2e` 包（见下节）。



## 端到端测试环境（先于任何业务修复建设）

在动手改业务代码前，先合入一个**仅测试基础设施**的提交（仍在同一分支），后续每个条目直接在其上加场景。环境设计贴合本仓库现有栈，不引入新运行时框架。

### 目标与边界
- 覆盖主链路：创建订阅 → SubscribeSearch → admit/add → pending → Transfer → 入库 / 洗版替换 / 删除旧文件。
- 错误与竞态：ffprobe 失败、qB 403、HTTP 种子匹配、pending 误删、时钟推进、双实例、下载器任务缺失。
- **不**依赖真实 qB/Tr/PT/TMDB 网络；全部用本机 `TcpListener` mock 或内存桩。

### 复用本仓库已有能力
| 能力 | 现有位置 | 用法 |
|---|---|---|
| mock qB / Transmission HTTP | `crates/downloader/tests/qbit.rs`、`transmission.rs`（`TcpListener` + `Capture`） | 抽到 `crates/downloader/src/test_support.rs`（`cfg(feature = "test-util")`）或 `crates/test-support`，供 api/subscribe 集成测试复用 |
| 内存下载器 | `downloader::MemoryDownloader` | 不需要协议层时的轻量路径 |
| 可注入时钟的 Job 循环 | `api::job_loop::spawn_job_loop_with_control`（`clock: Fn() -> i64`） | E2E 用 `Arc<AtomicI64>` / `Mutex` 推进时间，验证 15 分钟宽限期、播出日、无进度超时 |
| MediaProbe 可替换 | `library::MediaProbe` trait；`collect_completed_controlled(..., probe)` | `FakeProbe`：按路径返回固定质量 / 固定 Err |
| Catalog | `api::catalog::Catalog` trait | `FakeCatalog`：固定 `air_date` / 季集列表 |
| Indexer | `indexer` Search/RSS 结果结构 + 站点 profile | 本地 `TcpListener` 模拟 RSS/搜索 HTML，或测试用 `FakeIndexer` 直接返回 `Vec<Torrent>` |
| 临时库目录 | `tempfile`（workspace 已有） | 每用例独立 `TempDir`：下载暂存、媒体库、SQLite |
| 一键本地环境 | `./start-test.sh`（后端 + 前端 + mock 站点 + 可选真实 qB） | 人工冒烟；CI 不依赖它 |
| live-loop | `crates/api/src/bin/live_loop/` | 长期冒烟参考，E2E 不强制调用 |

### 建议新建结构
```
crates/test-support/          # 可选：公共 TcpListener mock、FakeProbe、FakeCatalog、时钟
crates/api/tests/subscription_e2e/
  harness.rs                  # 组装 ApiState：Store(temp) + FakeIndexer + mock DL + FakeProbe + FakeCatalog + 可控 clock
  scenarios/
    _template.rs
    d_s1_pending_cleanup.rs
    s1_wash_probe_fail.rs
    ...                       # 一文件一编号
```
若不单独建 crate，可把 harness 放在 `crates/api/tests/subscription_e2e_harness.rs`，用 `mod` 引入各场景文件。

### Harness 最小 API（示意）
- `Harness::new()`：临时数据目录、默认过滤规则组、启用 mock 下载器、挂 FakeCatalog。
- `create_tv_subscribe(media, coverage, wash_cut, …) -> SubscribeId`
- `seed_owned_file(slot, quality, bytes)`：写入媒体库 + ledger + facts
- `mock_indexer.set_results(Vec<Torrent>)` / `mock_qb.inject_infos(...)` / `mock_qb.set_progress(hash, f64)`
- `fake_probe.set(path, Result<FileQuality>)`
- `advance_clock(secs)`；`run_search(subscribe)`；`run_transfer(subscribe)`；`list_pending()`；`list_library_files()`
- `assert_file_hash(path, expected)`；`assert_pending_state(enclosure, "active|imported|rejected")`

### CI 跑法
- 现有 CI 若跑 `cargo test --workspace`，新增测试自动纳入；否则在 workflow 增加：
  `cargo test -p api --test subscription_e2e -- --test-threads=4`
- mock 服务器绑定 `127.0.0.1:0`，禁止硬编码端口；用例超时建议 30s。
- 不启动 Docker、不访问外网；失败时打印 mock 请求日志（`Capture` 已记录 adds/posts）。

### 并发与故障注入
- **并发**：同一 `ApiState` 上用 `std::thread`/`std::thread::scope` 并行跑 `run_search`/`run_transfer`；双实例用两个 `ApiState` 指向同一 SQLite 文件（覆盖 G-M5）。
- **故障注入**：mock 响应按请求序号返回 403/HTML；`FakeProbe` 按调用次数失败；文件系统用只读目录或 `fail` 注入点（仅测试 feature）模拟 EACCES。
- **时间**：禁止 `sleep` 等真实宽限期；一律 `advance_clock`。

### 建设顺序（先于 P0）
1. 抽出/复用 mock qB、Tr、FakeProbe、FakeCatalog、可控 clock，合并为 harness。
2. 落地一条黄金路径场景（无洗版：创建 → 搜索 → 提交 → 完成 → 入库）作为模板，确认 CI 绿。
3. 再开始 P0 各条目，每条在 `scenarios/` 下新增对应文件。



## P0：止血：消除数据丢失与在途任务误删

### D-S1. 任务列表清理永远匹配不上 HTTP 种子，15 分钟后误删在途 pending

- **严重度**：严重
- **涉及文件/函数**：`crates/api/src/http/downloaders/task_listing.rs` `snapshot_matches_pending`、`prune_absent_pending`、`task_row_json`；`crates/downloader/src/lib.rs` `TaskSnapshot`；`crates/downloader/src/qbit.rs` `our_tag`、`task_snapshots`；`crates/downloader/src/transmission.rs` `task_snapshots`
- **根因**：快照只保留通用标签 `crawler-media`（qB `our_tag` 显式排除 `crawler-media-owned-`；Tr `find` 先命中 `TASK_TAG`），而匹配要求 `snapshot.tag == ownership_tag(enclosure)`，HTTP enclosure 恒为假阴性 → `absent && stale` → `delete_pending`。
- **具体改法**：
  1. `TaskSnapshot` 新增 `owned_tags: Vec<String>`（保留全部 `crawler-media-owned-*`），`tag` 字段语义不变（仍用于区分自家/刷流）。
  2. qB `task_snapshots` 从 `info.tags` 解析全部标签填 `owned_tags`；Tr 从 `labels` 填。
  3. `snapshot_matches_pending`：磁力仍按 hash；HTTP 按 `owned_tags.contains(ownership_tag(enclosure))`；若 pending 已持久化 `info_hash`（见 D-M3）优先按 hash。
  4. 防御：`prune_absent_pending` 在“本下载器所有 HTTP pending 都 absent”时视为识别异常，只告警不删除（熔断）。
  5. 清理动作获取 `subscribe_guard`（顺带修 L6 部分）。
- **验收标准**：
  - 新增单测：qB/Tr 快照含 `crawler-media,crawler-media-owned-<sha>` 时 `snapshot_matches_pending` 为 true；不含时为 false。
  - 集成测试（MemoryDownloader 或 mock qB）：提交 HTTP 种子 → 时间推进 16 分钟 → 调 `list_tasks` → pending 仍为 active，任务状态非 `missing`。
  - 熔断测试：快照全部无 owned 标签时不删除任何行并打 error 日志。
- **流程测试场景**：创建 TV 订阅（HTTP enclosure）→ mock 索引站返回 S01E01 → 提交到 mock qB（任务带 `crawler-media,crawler-media-owned-<sha>`）→ 时钟前进 16 分钟 → 调用 `GET` 下载任务列表与后台对账 → mock qB 将进度置 100% → Transfer → 入库；同场景再跑一遍 mock Transmission（rpc≥16 带 labels）。异常分支：qB 快照全部无 owned 标签（触发熔断）、下载器不可达。
- **通过标准**：pending 全程为 active 直至入库后变 imported；任务列表状态为 downloading/completed 而非 missing；文件进入临时媒体库，ledger 1 行；熔断分支 0 行被删且有 error 日志；下载器 add 调用次数恰为 1。
- **风险与回滚**：风险：老版本客户端任务只有 legacy 标签，可能仍匹配不上——由熔断兜底。回滚：单独 commit，revert 即恢复；`TaskSnapshot` 字段为新增，`Default` 兼容，不涉及 DB 迁移。

### S1. probe 失败时洗版仍按标题批准并删除在位旧文件

- **严重度**：严重
- **涉及文件/函数**：`crates/subscribe/src/collect.rs` `collect_one`、`quality_for`、`replacement_survives_probe`、`commit_video_facts`
- **根因**：`quality_for` probe 失败回退标题质量（`QualitySource::Release`）；`replacement_survives_probe` 中 `safe_quality = !was_probed || …` 在未 probe 时跳过降级保护，后续 `is_replacement_approved` 仍用标题质量批准删除。
- **具体改法**：
  1. `replacement_survives_probe`：`wash_cut && owns_any && !was_probed` 时直接返回 `(false, 0)`，即不批准替换、新文件只作为并列文件登记（或按配置拒收并删除刚转存的临时目标）。
  2. 给 ledger 行标 `quality_source=Release, confidence=Low`（已有），并把该路径加入 probe 重试队列（`enqueue_probes_for_rows` 已存在）。
  3. 可选二期：probe 重试成功后再由后台评估是否执行替换（独立 job，持 guard）。
  4. 修正注释，使其与行为一致。
- **验收标准**：
  - 单测：mock `MediaProbe` 返回 Err，槽位有在位 2160p 文件，候选标题 2160p+更高分 → `removed_paths` 为空、facts 仍指向旧文件。
  - probe 成功路径既有测试（`tests/cutoff.rs`、`multi_slot_wash_cut.rs`）全部通过。
  - 日志出现“probe 失败，拒绝洗版替换”。
- **流程测试场景**：订阅开启洗版（keep_old_versions=false），库中已有 S01E01 2160p（mock ffprobe 返回 2160p）→ 搜索返回标题为 2160p DV 的高分候选 → 提交 → 完成 → Transfer 时 mock ffprobe 对新文件返回错误/超时；对照组：mock ffprobe 返回真实 2160p DV。
- **通过标准**：probe 失败组：旧文件仍存在、facts 指向旧文件、`removed_paths` 为空、新文件以并列版本登记为低置信度且进入 probe 重试；对照组：按原逻辑替换并删除旧文件。
- **风险与回滚**：风险：ffprobe 缺失环境中洗版将不再生效（预期行为，需在 UI/日志提示）。回滚：revert 单个 commit；无数据迁移。

### D-H3. 最终发布 `fs::rename` 静默覆盖已存在的目标文件

- **严重度**：高
- **涉及文件/函数**：`crates/library/src/file_transfer.rs` `transfer_file_controlled`
- **根因**：Unix `rename` 遇到已存在 dest 原子覆盖；`dest_path` 的存在性检查与 rename 之间有 TOCTOU 窗口。
- **具体改法**：
  1. 发布步骤改为不覆盖语义：Linux 用 `renameat2(..., RENAME_NOREPLACE)`（`nix`/`libc`）；不支持时回退 `fs::hard_link(temp, dest)`（EEXIST 即失败）+ `remove_file(temp)`。
  2. 返回专用错误 `LibraryError::DestinationExists`，由调用方（M8）选择新名重试。
  3. 保留 src==dest/规范路径相同 no-op 逻辑。
- **验收标准**：
  - 单测：dest 预先存在内容 A，调用 transfer 写 B → 返回 `DestinationExists`，dest 内容仍为 A，临时文件已清理。
  - Copy/Hardlink/Move 三模式正常路径测试通过；跨设备回退路径通过。
- **流程测试场景**：库目录预置 `Show - S01E01.mkv`（内容 A）；订阅收集一个渲染到同名路径的新文件（内容 B），分别在 Hardlink/Copy/Move 模式及跨设备（tmpfs 与磁盘两个临时目录）下运行；并发组：两个线程同时向同一 dest 发布。
- **通过标准**：任何模式下内容 A 不被覆盖；发布失败返回 DestinationExists 并由调用方改名重试成功；临时 `.transfer-*` 文件无残留；并发组只有一个写入成功，另一个改名。
- **风险与回滚**：风险：部分文件系统（某些 FUSE/SMB）不支持 `RENAME_NOREPLACE` → 走 hard_link 回退，再不支持则失败报错（宁可失败不覆盖）。回滚：revert；无数据迁移。

### M8. `dest_path` 碰撞只改名一次，二次碰撞与 D-H3 叠加覆盖在位文件（技术复核由中升为高）

- **严重度**：高
- **涉及文件/函数**：`crates/subscribe/src/collect.rs` `dest_path`、`collect_one`
- **根因**：碰撞时只生成 `{stem}-{src_stem}.{ext}` 一次且不再检查是否存在；并列版本/重复收集会命中同一备用名。
- **具体改法**：
  1. `dest_path` 循环生成候选：`{stem}-{src_stem}.{ext}`、`{stem}-{src_stem}-2.{ext}` … 上限 100，超出报错。
  2. `collect_one` 捕获 `DestinationExists`（D-H3）后重新计算 dest 重试一次，避免竞态。
  3. 若候选已存在且与 src 为同一 inode/大小+哈希一致，直接复用并视为已收集（幂等）。
- **验收标准**：
  - 单测：canonical 与 `-src_stem` 均存在时得到 `-2` 后缀，且两个已有文件内容不变。
  - 同一 src 重复 collect 不产生第三个副本。
- **流程测试场景**：同一集依次收集 3 个不同来源文件（canonical、`-src_stem` 均已存在），以及同一源文件重复收集两次。
- **通过标准**：得到 `-2` 等递增后缀，已有文件哈希不变；重复收集不产生新副本，ledger 行数不增加。
- **风险与回滚**：风险：文件名后缀增多，媒体服务器可能识别为多版本（已有行为）。回滚：revert；无迁移。


## P1：闭环：占位/去重/客户端稳定性

### H1. 洗版期间同一槽位重复下载多个升级种子

- **严重度**：高
- **涉及文件/函数**：`crates/api/src/worker/finish_search.rs` `facts_with_pending`；`crates/api/src/management/subscribes/run.rs` `load_run_facts`；`crates/subscribe/src/choose.rs` `should_replace_slots`；`crates/subscribe/src/facts.rs`
- **根因**：在途 pending 只在槽位为空时占位；洗版槽位已有文件时在途升级不影响选种，仅按 enclosure 排除同一种子。
- **具体改法**：
  1. `SubscribeFacts` 新增 `in_flight: HashMap<slot, Release>`（不持久化，`retain_owned` 不涉及）。
  2. 共享占位函数（见 H4）对已有槽位写 `in_flight` 为 pending 标题质量。
  3. `should_replace_slots`：槽位存在 `in_flight` 时，基准取 `max(在位质量, in_flight 质量)`（ladder 比较）；无 ladder 时直接 `false`（同一槽位最多一个在途洗版）。
- **验收标准**：
  - 单测：E01 在位 1080p，pending 中有 2160p 升级 → 另一个 2160p 不同 enclosure 候选不被选中；更高（2160p DV，ladder 含 hdr）候选按配置可选/不可选。
  - 集成：连续两轮 finish_search，下载器 add 次数为 1。
- **流程测试场景**：洗版订阅，E01 在位 1080p；第 1 轮搜索选中 2160p 种子 A（下载中，mock qB 进度 30%）；第 2、3 轮 mock 索引站再返回同质量不同 enclosure 的 B、C；随后 A 完成并入库。
- **通过标准**：第 2、3 轮下载器 add 次数为 0；A 入库后旧文件按规则替换；全过程只发出 1 次“命中新资源”通知。
- **风险与回滚**：风险：pending 卡死时会阻塞洗版——依赖 H3 后台清理。回滚：revert；内存结构无迁移。

### H2. probe 否决的洗版种子被反复选中、提交、否决

- **严重度**：高
- **涉及文件/函数**：`crates/subscribe/src/collect.rs` `replacement_survives_probe`、`collect_scored_torrent`；`crates/store/src/subscribes.rs` `mark_pending_imported`、`upsert_pending_row`、`load_pending`；`finish_search.rs` `remove_pending_candidates`；`run.rs` `exclude_pending`
- **根因**：否决后 pending 置 `imported`，排除逻辑只读 `active`；`upsert_pending_row` 冲突时把 state 改回 `active`。
- **具体改法**：
  1. `RunOutcome` 新增 `rejected_enclosures`，collect 中 probe 否决的种子加入。
  2. Store 新增 `mark_pending_rejected`（state=`rejected`，记录原因/实际质量）。
  3. 排除逻辑改为读取 `active|imported|rejected` 全部 enclosure（新增 `load_pending_enclosures_any_state`）。
  4. `upsert_pending_row`：已是 `rejected` 时不复活（`WHERE state != 'rejected'`），手动投递除外（显式参数）。
- **验收标准**：
  - 集成：模拟 probe 否决后再跑 3 轮 finish_search，下载器 add 次数为 0，无“订阅命中新资源”通知。
  - 手动投递同 enclosure 仍可成功并复活为 active。
- **流程测试场景**：洗版订阅，候选标题 2160p，下载完成后 mock ffprobe 返回 1080p（否决）；再连续运行 3 轮定时搜索；之后通过手动投递再提交同一 enclosure。
- **通过标准**：pending 状态为 rejected；3 轮内 add 次数 0、无新通知；手动投递成功且状态复活为 active；库中只有 1 个并列文件。
- **风险与回滚**：风险：误判 rejected 的种子不再自动重试——提供 UI/API 清除 rejected。回滚：revert；新增 state 值对旧代码表现为“非 active”，兼容。

### H3. 失效 pending 只在 UI 接口清理，后台从不清理导致槽位永久阻塞

- **严重度**：高
- **涉及文件/函数**：`crates/api/src/http/downloaders/task_listing.rs` `prune_absent_pending`；`crates/api/src/worker/transfer.rs` `transfer_one`；`crates/api/src/worker.rs` `transfer`
- **根因**：缺失检测只在 `list_tasks` 中执行（且因 D-S1 有害）；定时任务从不对齐下载器状态。
- **具体改法**：
  1. 把 `prune_absent_pending` 抽为 `api::pending_reconcile` 模块，供 Transfer job 每轮调用（每个下载器拉一次快照，见 D-L2）。
  2. 在 `transfer_one` 持 guard 时对本订阅执行；`list_tasks` 只读展示不再删除（或保留但同样持 guard）。
  3. 新增“无进度超时”（如 7 天进度不变/死种）→ 标记 `stalled` 并释放占位，可触发换源（复用 `exclude_enclosure`）。
  4. 依赖 D-S1 先合入。
- **验收标准**：
  - 集成：下载器中删除任务 → 16 分钟后 Transfer job 运行 → pending 被清理，下一轮搜索可重新选源；不打开 UI 也生效。
  - 在途正常任务不被清理（D-S1 用例复用）。
- **流程测试场景**：提交种子后在 mock qB 中删除该任务（模拟用户手删）→ 时钟前进 16 分钟 → 只运行后台 Transfer Job（不调用 UI 接口）→ 下一轮搜索；另组：任务长期 0 进度超过无进度超时。
- **通过标准**：后台对账删除/标记 stalled 的 pending；下一轮搜索能选中其他源并提交；正常下载中的任务不被清理。
- **风险与回滚**：风险：误清理会导致重复下载——熔断与 grace 保留。回滚：关闭后台调用的配置开关（新增 `subscribe.pending_reconcile_enabled`），或 revert。

### H4. 整季包 pending 占位口径与选种不一致，导致重复下载

- **严重度**：高
- **涉及文件/函数**：`run.rs` `load_run_facts`；`finish_search.rs` `facts_with_pending`、`covered_units`；`crates/api/src/worker.rs` `covered_units`；`crates/api/src/open_coverage.rs`；`crates/subscribe/src/choose.rs` `covered_slots`
- **根因**：三处各自实现占位展开，手动运行路径缺 D04 修复，纯季包只占 `(s, None)`。
- **具体改法**：
  1. 在 `subscribe` crate 新增 `pub fn pending_slots(subscribe, release) -> Vec<slot>`，内部调用 `covered_slots`，并对纯季包（任意模式）展开为订阅窗口全部集。
  2. 新增 `pub fn facts_with_pending(facts, pending, subscribe, media)`，`run.rs` 与 `finish_search.rs` 共用（同时承载 H1 的 in_flight）。
  3. 删除重复实现；`worker::covered_units` 仅用于 wanted 记录时也改为调用共享函数。
- **验收标准**：
  - 单测：纯季包 `S01` pending 在 full_season_pack 开/关、手动/定时两路径下均占满 E01..En。
  - 回归：`crates/subscribe/tests/*`、`crates/api/tests/management/subscribes.rs` 全通过。
- **流程测试场景**：full_season_pack 开/关两种订阅，pending 中存在纯季包 `Show.S01.1080p`（未完成）；分别触发手动“立即运行”和定时搜索，mock 索引站同时返回另一个季包与单集。
- **通过标准**：两条路径下均不再提交新的季包或单集；`facts_with_pending` 占满 E01..En；add 次数为 0。
- **风险与回滚**：风险：非 full_season_pack 订阅中，误解析为季包的 pending 会占满整季（受 L1 影响）——与 L1 一并处理。回滚：revert。

### D-H1. qBittorrent 会话过期后不重新登录

- **严重度**：高
- **涉及文件/函数**：`crates/downloader/src/qbit.rs` `connect`、`get`、`post_form`、`post_add_form`、`set_speed_limits`；`crates/api/src/dynamic_downloader.rs` `client`
- **根因**：cookie 只在 `connect` 获取一次；403 直接当错误；`DynamicDownloader` 在配置不变时永久缓存客户端。
- **具体改法**：
  1. `cookie` 改为 `Mutex<String>`；抽 `fn request_with_relogin(f)`：响应 403 时调用 `login()` 刷新 cookie 后重试一次。
  2. 所有 HTTP 方法经由该包装（ureq 需关闭 `http_status_as_error` 或匹配 `Error::StatusCode(403)`）。
  3. `DynamicDownloader`：调用返回认证类错误时清除缓存。
- **验收标准**：
  - mock HTTP 服务：第一次 info 返回 403、login 后返回 200 → `completed_files` 成功且只重登一次。
  - 连续 403（密码错误）→ 返回明确错误，不无限重试。
- **流程测试场景**：mock qB 在登录后第 N 次请求开始返回 403（模拟 SID 过期），随后接受重新登录；流程：搜索提交 → 403 → Transfer 收集；异常组：重新登录也失败（密码被改）。
- **通过标准**：提交与收集在一次自动重登后成功，login 请求恰好多 1 次；密码错误组返回明确认证错误，不无限重试，Job 标记失败。
- **风险与回滚**：风险：并发请求同时重登——用 Mutex 串行化登录。回滚：revert。

### D-H2. 下载器已有同 infohash（非本程序添加）任务时提交永久失败

- **严重度**：高
- **涉及文件/函数**：`crates/downloader/src/qbit.rs` `add_with_options`、`exact_existing_hash`、`post_add_form`；`transmission.rs` `add_with_options`
- **根因**：HTTP enclosure 仅按 owned 标签去重，无法识别已存在的同 infohash 任务；qB 返回 `Fails.` 当硬错误。
- **具体改法**：
  1. 新增 `downloader::torrent_info_hash(bytes) -> Option<String>`：解析 bencode 的 `info` 字典原始字节做 SHA-1（引入 `serde_bencode` 或手写最小解析）。
  2. qB：下载 .torrent 后先算 hash，查 `/torrents/info?hashes=`；已存在则 `/torrents/addTags` 补 `crawler-media,owned` 标签并视为成功（认领），并检查状态（G-M2）。
  3. Tr：`torrent-duplicate` 时调用 `torrent-set` 补 labels。
  4. 把 hash 返回给调用方持久化到 pending（D-M3）。
- **验收标准**：
  - 单测：bencode hash 计算与已知种子一致。
  - mock qB：已存在同 hash 无标签任务 → add 返回 Ok、调用了 addTags、随后 `completed_files` 可认领。
- **流程测试场景**：mock qB 中预置一个同 infohash、无本程序标签的任务（模拟用户手动添加）；订阅搜索到该 .torrent（mock 站点返回真实 bencode 种子）→ 提交 → 任务完成 → Transfer；Transmission 组使用 `torrent-duplicate` 响应。
- **通过标准**：提交返回成功且调用了 addTags/torrent-set；pending 记录了 info_hash；文件被认领并入库；不再每轮报 `Fails.`。
- **风险与回滚**：风险：认领用户手工添加的任务后，后续 Move/清理可能影响用户自有任务——认领时只加标签，破坏性操作仍需 owned 证明；可加配置“允许认领外部任务”默认开。回滚：revert。


## P2：正确性：选剧、准入、事务与多实例

### G-H1. 开放窗口无播出日门控，未播集照常周期搜索

- **严重度**：高
- **涉及文件/函数**：`crates/api/src/open_coverage.rs` `units`；`crates/api/src/worker.rs` `search`；`crates/api/src/catalog_cache.rs`（air_date）；`crates/api/src/http/subscribe_schedule.rs`
- **根因**：开放窗口靠“已知最高集+1”，不读 Catalog 播出日；搜索 Job 固定间隔全站搜索。
- **具体改法**：
  1. 新增 `air_schedule(subscribe, media) -> AirInfo{ next_air: Option<date>, aired_max_ep }`，读取 catalog_cache 集 `air_date`。
  2. `worker::search` 前判断：所有待追集均未播出 → 跳过本轮并把下一次 run_after 设为 `next_air`（或保留原间隔但不查站点）。
  3. 播出后 48h 内使用更短间隔（可配置），之后恢复。
  4. 无 Catalog 数据时保持现有行为。
- **验收标准**：
  - 单测：next_air 在未来 → search 不调用 indexer；播出日已过 → 正常搜索。
  - 集成：Job run_after 被正确推迟。
- **流程测试场景**：开放窗口订阅（episode_to=None），mock TMDB 返回 E05 播出日为“当前时钟+3 天”；时钟前进 1 天/4 天分别运行搜索 Job。
- **通过标准**：未播出期间 mock 索引站搜索请求次数为 0 且 Job run_after 推迟到播出日；播出后立即搜索并提交 E05。
- **风险与回滚**：风险：Catalog 播出日错误导致漏搜——提供“忽略播出日”开关，且最长推迟不超过 7 天。回滚：开关关闭即恢复原行为。

### G-H2. 标题解析缺季时默认 S01，无目录校验即入库错误槽位

- **严重度**：高
- **涉及文件/函数**：`crates/release/src/lib.rs`（默认 `season = Some(1)` 处）；`crates/subscribe/src/choose.rs` `candidate_matches_subscribe`、`covered_slots`；`crates/api/src/worker/transfer.rs` `filter_matched_pending`；`crates/subscribe/src/file_identity.rs`
- **根因**：解析器在缺季信息时默认第 1 季，订阅链路完全信任标题解析结果。
- **具体改法**：
  1. `Release` 增加 `season_inferred: bool`，默认 S01 时置 true（不改原字段值以减小影响面）。
  2. `candidate_matches_subscribe`：`season_inferred` 且订阅季≠1，或 Catalog 显示该剧多季且集号超出 S01 集数时拒绝；可选：按 Catalog 绝对集号映射到真实季集。
  3. 洗版删除（S1 路径）对 `season_inferred` 候选一律不批准替换。
- **验收标准**：
  - 单测：`Show Episode 13` 对多季剧（S01 共 12 集）不进入 S01 槽位；单季剧仍可匹配。
  - 既有 release 解析测试不回归。
- **流程测试场景**：订阅多季剧 S02（mock TMDB：S01 共 12 集），mock 索引站返回 `Show Episode 13 1080p`（无季标识）与 `Show S02E01`；完成后 Transfer。
- **通过标准**：无季标识的资源不进入 S02/S01 错误槽位、不触发洗版删除；S02E01 正常入库。
- **风险与回滚**：风险：动漫绝对集号资源匹配率下降——映射功能作为二期。回滚：revert；`season_inferred` 为新增字段。

### M1. 按集独立择优导致重叠季包与单集同时下载

- **严重度**：中
- **涉及文件/函数**：`crates/subscribe/src/choose.rs` `choose`（Tv 分支）
- **根因**：`best_by_ep` 每集独立取最高分再按 enclosure 去重，不考虑候选之间覆盖重叠。
- **具体改法**：
  1. 改为贪心覆盖：候选按 `(score desc, 覆盖空槽数 desc)` 排序；维护 `claimed` 集合；候选只有在其覆盖槽位中至少一个未被 claimed 且在这些未 claimed 槽位上不劣于其它候选时入选，入选后 claim 全部覆盖槽位。
  2. 增加策略配置 `prefer_pack | prefer_episode`（默认 prefer_pack）。
- **验收标准**：
  - 单测：包 E01-E10(分 90) + 单集 E06(分 95) → 只选其一（按策略）；不重叠单集各自入选。
  - `choose/tests.rs` 既有用例更新/通过。
- **流程测试场景**：TV 订阅 E01-E10，mock 索引站同时返回包 E01-E10（分 90）与单集 E06（分 95）；分别以 prefer_pack / prefer_episode 运行完整流程至入库。
- **通过标准**：每个策略下只有一组资源被提交；库中每集只有一个文件；无重复下载。
- **风险与回滚**：风险：选择结果变化可能让部分集等待更久。回滚：revert。

### M2. 非洗版下覆盖已拥有集的范围包仍被选中并产生库内重复

- **严重度**：中
- **涉及文件/函数**：`crates/subscribe/src/choose.rs` `should_replace_slots`；`crates/subscribe/src/collect.rs` `collect_one`、`commit_video_facts`
- **根因**：`any` 语义下有一集缺失就选整包；收集时非洗版不拦截已拥有槽位的文件，`upsert` 改指针不删旧文件。
- **具体改法**：
  1. `collect_one`：`!wash_cut && 所有 slots 已有在位文件` → `Ok(None)`（不转存）。
  2. 非洗版 `commit_video_facts` 对已有槽位不再 upsert（只填空槽）。
  3. 与 M1 配合，选种阶段优先只覆盖缺失集的资源。
- **验收标准**：
  - 集成：E01 已在库，包 E01-E02 完成 → 只转存 E02，库中 E01 只有一份，facts 不变。
- **流程测试场景**：非洗版订阅，E01 已在库；mock 索引站只有包 E01-E02；下载完成后 Transfer。
- **通过标准**：只转存 E02；E01 在库文件和 facts 不变；ledger 不新增 E01 行。
- **风险与回滚**：风险：用户想保留多版本的场景需开 `keep_old_versions`+洗版。回滚：revert。

### M3. 洗版开启时缺集补全也只用洗版规则组准入

- **严重度**：中
- **涉及文件/函数**：`crates/subscribe/src/add.rs` `admit_and_add`
- **根因**：`wash_cut && wash_filter.is_some()` 时所有候选统一用洗版规则组准入。
- **具体改法**：
  1. 分两次准入：主规则组准入结果中仅保留“覆盖至少一个空槽”的候选；洗版规则组准入结果中仅保留“覆盖已有槽”的候选；合并去重（同 enclosure 取较高分）。
  2. `choose` 中空槽比较用主规则组分数，已有槽用洗版分数（ScoredTorrent 增加 `wash_score: Option<i32>`）。
- **验收标准**：
  - 单测：洗版规则只收 2160p，缺失 E03 仅有 1080p → E03 被选中；已有 E01 不被 1080p 替换。
- **流程测试场景**：洗版规则组仅接受 2160p；E01 在库 1080p，E03 缺失；mock 索引站返回 E01 2160p 与 E03 1080p。
- **通过标准**：E01 2160p 与 E03 1080p 都被提交并入库；E01 按洗版替换，E03 按主规则组补齐。
- **风险与回滚**：风险：分数尺度混用——通过分字段解决。回滚：revert。

### M6. Transfer 落库非原子，删旧文件失败后不再重试

- **严重度**：中
- **涉及文件/函数**：`crates/api/src/worker/transfer.rs` `commit_transfer_outcome`；`crates/api/src/management/subscribes/run.rs` `persist`；`crates/store/src/subscribes.rs` `save_subscribe_facts`；`crates/api/src/http/file_delete.rs`
- **根因**：ledger/facts/pending/wanted 多步独立写；物理删除夹在中间，失败提前返回且下轮不会再生成 `removed_paths`。
- **具体改法**：
  1. 新增表 `pending_file_deletions(path, ledger_id, reason, attempts, next_retry_at)`。
  2. commit 在单个事务中：插 ledger、写 facts（含 `persist_owned_quality`）、标 wanted/pending、把 `removed_paths` 写入待删除表。
  3. 事务提交后，锁外执行物理删除；成功则删表行+ledger；失败则保留由后台 job（可挂 Transfer）重试。
  4. `run.rs::persist` 复用同一函数。
- **验收标准**：
  - 故障注入：删除时 EACCES → facts 指向新文件，待删除表有记录；权限恢复后下一轮删除成功、ledger 清理。
  - 进程在事务中途 kill → 重启后 DB 无半写状态。
- **流程测试场景**：洗版替换流程中，对旧文件所在目录设置只读（EACCES）使删除失败 → 完成 Transfer → 恢复权限 → 再跑一轮 Transfer；另组：在提交事务中途 kill 进程（测试中用故障注入点 panic）。
- **通过标准**：删除失败时 facts 指向新文件、待删除表有记录；恢复后旧文件和 ledger 被清理；故障注入组重启后 DB 无半写状态。
- **风险与回滚**：风险：需要 schema 迁移。回滚：迁移为新增表，旧版本忽略；代码 revert 后待删除表残留无害。

### G-M2. `exact_existing_hash` 短路成功但不检查任务存活状态

- **严重度**：中
- **涉及文件/函数**：`crates/downloader/src/qbit.rs` `add_with_options`；`crates/subscribe/src/add.rs`
- **根因**：已存在同 owned 标签任务即返回 Ok，不论其是否暂停/报错/缺文件；调用方照常写 active pending。
- **具体改法**：
  1. 短路分支读取该任务 state：`error|missingFiles` → 返回特定错误 `ExistingTaskBroken`；`paused*`（未完成）→ 调 resume。
  2. 调用方收到 `ExistingTaskBroken` 时不写 pending，并把 enclosure 记入 rejected（H2）以触发换源。
- **验收标准**：
  - mock qB：已有 error 状态任务 → add 返回 `ExistingTaskBroken`，pending 未写入；paused 任务被 resume。
- **流程测试场景**：mock qB 中已存在同 owned 标签任务，状态分别为 error/missingFiles/pausedDL；订阅搜索命中该 enclosure。
- **通过标准**：error/missingFiles 组不写 pending、enclosure 标 rejected，并在下一轮换源提交；pausedDL 组被 resume 且 pending 正常。
- **风险与回滚**：风险：误判状态字符串（qB 版本差异）——状态映射集中在一个函数并加测试。回滚：revert。

### G-M5. 订阅互斥仅进程内 Mutex，多实例并发选种/入库

- **严重度**：中
- **涉及文件/函数**：`crates/api/src/management/state.rs` `subscribe_guard`；`crates/jobs/src/*`
- **根因**：`subscribe_guards` 为进程内结构，无 DB 级租约。
- **具体改法**：
  1. 短期：在部署文档与启动日志中声明仅支持单实例；启动时在 DB 写入 `instance_lock`（含心跳），检测到另一活跃实例则拒绝启动或只读运行。
  2. 长期：新增 `subscribe_leases(subscribe_id, owner, expires_at)`，`subscribe_guard` 获取进程锁后再 `UPDATE … WHERE expires_at < now OR owner = me` 获取租约。
- **验收标准**：
  - 启动两个实例指向同一 DB → 第二个实例报错退出（或进入只读）。
  - 单实例行为不变。
- **流程测试场景**：同一 SQLite 数据目录启动两个 ApiState（或两个进程）并同时运行搜索与 Transfer。
- **通过标准**：第二个实例拒绝启动或进入只读；不会出现重复提交与双重入库；锁在心跳超时后可被接管。
- **风险与回滚**：风险：异常退出遗留锁——心跳超时自动释放。回滚：开关 `allow_multi_instance=true` 跳过检测。


## P3：其余中低项、设计取舍与体验

### M4a. 已完成/已达标的订阅仍按周期全站搜索

- **严重度**：中
- **涉及文件/函数**：`crates/api/src/worker.rs` `search`；`crates/api/src/worker/finish_search.rs`；`crates/api/src/http/subscribe_schedule.rs`；`crates/domain/src/lib.rs` `tracking_state`
- **根因**：没有 completed 状态，完成后 Job 不停。
- **具体改法**：
  1. `tracking_state` 增加 `completed`。
  2. 非洗版有界订阅在 Transfer 后 `is_complete` 为真 → 置 completed 并 `sync_search_job(paused=true)`；洗版订阅所有槽位 `target_reached` → completed。
  3. 文件删除/范围扩大/洗版规则变更时恢复 active。
- **验收标准**：
  - 集成：全部入库后搜索 JobDef 被禁用；删除一集后恢复 active 并调度。
- **流程测试场景**：有界订阅 E01-E02 全流程入库 → 继续推进时钟 3 个搜索周期 → 删除 E02 文件。
- **通过标准**：入库完成后状态为 completed、搜索 JobDef 禁用、索引站 0 请求；删除 E02 后恢复 active 并重新搜索提交。
- **风险与回滚**：风险：UI 需识别新状态。回滚：将 completed 视同 active 的兼容代码保留一个版本。

### M4b. 洗版/开放窗口订阅永不 complete（技术复核：设计意图，降为低）

- **严重度**：低
- **涉及文件/函数**：`crates/subscribe/src/choose.rs` `is_complete`
- **根因**：洗版与开放窗口按设计持续追踪。
- **具体改法**：
  不改行为；在 `is_complete` 注释与 UI 中说明；完成判定交由 M4a 的 `target_reached` 逻辑。
- **验收标准**：
  - 文档/UI 文案更新；代码无行为变更。
- **流程测试场景**：洗版订阅全部达到 WashTarget 后观察状态与文案。
- **通过标准**：行为与 M4a 一致（达标即 completed）；`is_complete` 文档与 UI 文案已更新，无其它行为变化。
- **风险与回滚**：无风险。

### M5. `follow_future` 字段未被任何业务逻辑使用

- **严重度**：中
- **涉及文件/函数**：`crates/domain/src/lib.rs`；`crates/api/src/http/subscriptions/{create,patch_fields,views}.rs`；`open_coverage.rs`
- **根因**：字段只在 store/HTTP 读写，选种与调度不读取。
- **具体改法**：
  1. 明确语义：“开放窗口随播出扩展本季”，并与 G-H1 播出日门控绑定：`follow_future=false` 时开放窗口只追 Catalog 已播集。
  2. 跨季部分见 G-M3。
  3. 若短期不实现，UI 隐藏该开关。
- **验收标准**：
  - 单测：follow_future=false 时 `open_coverage::units` 不超过已播最大集；true 时包含 next+1。
- **流程测试场景**：开放订阅分别设置 follow_future=true/false，mock TMDB 已播至 E03、E04 未播，运行搜索与 wanted 记录。
- **通过标准**：false 时搜索/wanted 不超过 E03；true 时包含 E04 并按 G-H1 门控。
- **风险与回滚**：风险：行为变化影响现有开放订阅——默认值迁移为 true 保持现状。回滚：revert。

### M7. Transfer 放弃提交时已转存文件成为孤儿

- **严重度**：中
- **涉及文件/函数**：`crates/api/src/worker/transfer.rs` `transfer_one`、`commit_transfer_outcome`
- **根因**：转存后才检查订阅删除/租约，放弃时不登记不回滚。
- **具体改法**：
  1. `collect_completed_controlled` 前再检查一次 deleting/lease。
  2. 放弃提交时：Hardlink/Copy 模式删除本轮新建 dest；Move 模式把文件写入 `orphan_files` 表待处理（不删，避免丢数据）。
- **验收标准**：
  - 集成：collect 期间标记 deleting → 本轮新增 dest 被删除（Hardlink），库内无无主文件。
- **流程测试场景**：Transfer 收集过程中（mock ffprobe 内设置回调）标记订阅为 deleting。
- **通过标准**：本轮新建的库文件被回滚（Hardlink/Copy 删除；Move 登记到 orphan 表）；DB 无该订阅新 ledger。
- **风险与回滚**：风险：误删——只删除本轮 ledger 中记录的新路径。回滚：revert。

### M9. 无升级阶梯时按分数比较，分数尺度不一致

- **严重度**：中
- **涉及文件/函数**：`crates/subscribe/src/choose.rs` `should_replace_slots`；`crates/store/src/subscribe_library.rs`；`collect.rs` `replacement_survives_probe`
- **根因**：在位分数来源混杂（旧规则组/洗版规则组/扫描入库为 0），规则组变更不重算。
- **具体改法**：
  1. 内置默认 ladder `resolution,hdr,codec,source`，无配置时使用；分数仅作同质量 tie-break。
  2. 规则组更新时异步重算相关订阅在位分数。
- **验收标准**：
  - 单测：在位分数 0 但质量 2160p，候选 1080p 高分 → 不选中。
- **流程测试场景**：洗版订阅，在位文件为扫描入库（filter_score=None，ffprobe 2160p），无 ladder 配置；mock 索引站返回 1080p 高分候选。
- **通过标准**：候选不被提交；默认 ladder 生效；配置切回“按分数”后恢复旧行为。
- **风险与回滚**：风险：洗版触发频率变化。回滚：配置项切回“按分数”。

### D-M1. Move 模式复制后删源，破坏做种且同盘全量复制

- **严重度**：中
- **涉及文件/函数**：`crates/library/src/file_transfer.rs` `transfer_file_controlled`
- **根因**：Move 走 `copy_controlled` 再 `remove_file(src)`。
- **具体改法**：
  1. 同设备时 Move 直接 `rename`（NOREPLACE，见 D-H3）。
  2. 订阅路径（PT 站点来源）默认禁止 Move，设置页强提示；需要时 Move 后调用 `remove_owned(delete_files=false)` 移除任务。
- **验收标准**：
  - 单测：同设备 Move 不产生复制（inode 一致）；PT 订阅配置 Move 时保存被拒或告警。
- **流程测试场景**：Move 模式，下载目录与库同设备/跨设备两组，完成一个订阅入库；PT 订阅配置 Move 时保存设置。
- **通过标准**：同设备 Move 为 rename（inode 不变、耗时极短）；PT 订阅 Move 被拒或告警；跨设备行为与配置一致。
- **风险与回滚**：风险：用户依赖旧行为。回滚：开关。

### D-M2. 硬链接失败静默回退复制

- **严重度**：中
- **涉及文件/函数**：`crates/library/src/file_transfer.rs`
- **根因**：`hard_link` 失败仅 warn 即 copy。
- **具体改法**：
  新增库配置 `hardlink_fallback: copy|fail`（默认 copy 保持兼容）；回退次数计入 Transfer 结果与通知；设置页检测下载目录与库是否同设备。
- **验收标准**：
  - 单测：fail 模式下跨设备返回错误；copy 模式产生告警计数。
- **流程测试场景**：下载目录与库位于不同设备（两个临时挂载/不同 tmpfs），Hardlink 模式入库，分别配置 fallback=copy/fail。
- **通过标准**：copy 组成功且 Transfer 结果中回退计数 1 并有通知；fail 组入库失败并给出明确错误。
- **风险与回滚**：风险低。回滚：配置。

### D-M3. Transmission 身份映射仅在内存，重启/新建客户端后丢失

- **严重度**：中
- **涉及文件/函数**：`crates/downloader/src/transmission.rs` `add_with_options`、`owned_identity`；`crates/api/src/delivery.rs` `client_for_id`；`crates/store/src/subscribes.rs` pending JSON
- **根因**：HTTP enclosure 的 hash 只保存在进程内 `identities`；rpc<16 无 labels。
- **具体改法**：
  1. `Downloader::add` 返回 `Option<String>` hash（新增 `add_tracked` 方法，默认实现调用 `add` 返回 None）。
  2. pending JSON 增加 `info_hash` 并持久化；`owned_identity`/`completed_files` 优先使用持久化 hash。
  3. qB 同样提交后查询 hash 落库（配合 D-H2）。
- **验收标准**：
  - 集成：Tr（无 labels）提交 → 进程重启 → Transfer 仍能收集。
- **流程测试场景**：mock Transmission（rpc<16，无 labels）提交 HTTP 种子 → 重建 ApiState（模拟重启）→ 完成 → Transfer。
- **通过标准**：重启后仍能通过持久化的 info_hash 收集入库。
- **风险与回滚**：风险：JSON 字段新增，旧数据无 hash 时走原逻辑。回滚：revert。

### D-M4. .torrent 下载结果未校验；临时文件不清理

- **严重度**：中
- **涉及文件/函数**：`crates/downloader/src/qbit.rs` `add_with_options`、`download_torrent_bytes`、`http_agent`
- **根因**：≥8 字节即当种子；临时文件无删除；不走代理、5s 超时。
- **具体改法**：
  1. 校验 bencode 首字节 `d` 且可解析出 `info`，否则视为失败并回退 URL 提交（非签名 URL）或报“站点 Cookie 失效”。
  2. RAII guard 删除临时文件。
  3. 使用站点代理配置与可配置超时（默认 20s）。
- **验收标准**：
  - 单测：HTML 内容被拒；提交后 temp 目录无残留文件。
- **流程测试场景**：mock 站点下载链接分别返回 HTML 登录页、HTTP 403、合法 .torrent；提交到 mock qB。
- **通过标准**：HTML/403 组不上传伪种子，按规则回退 URL 或报“站点 Cookie 失效”；合法组上传成功；temp 目录无 `cm-torrent-*` 残留。
- **风险与回滚**：风险低。回滚：revert。

### D-M5. 手动投递绕过订阅锁与选种约束，pending 分数固定 0

- **严重度**：中
- **涉及文件/函数**：`crates/api/src/http/downloaders/submission.rs` `submit`、`record_submitted_pending`、`build_submit_torrent`
- **根因**：手动投递不持 guard、不检查在位/在途、score 写死 0、site_id 解析失败生成随机 id。
- **具体改法**：
  1. 带 subscribe_id 时在 blocking 闭包内获取 `subscribe_guard`；检查槽位在位/在途并返回 409（可 `force=true` 覆盖）。
  2. 用订阅规则组对 torrent 打分写入 pending。
  3. site_id 非法时返回 400 而非随机 id。
  4. 写 pending 失败时尝试 `remove_owned` 撤销提交。
- **验收标准**：
  - 集成：已在库的集手动投递返回 409；force 后成功；pending score 与规则组一致。
- **流程测试场景**：对已在库的 E01 通过 HTTP 接口手动投递；同时并发运行该订阅 Transfer；再用 force=true 投递。
- **通过标准**：首次返回 409；并发下无重复 pending 写入/竞态；force 后 pending 分数等于规则组打分。
- **风险与回滚**：风险：用户体验变化（多一步确认）。回滚：revert。

### G-M1. 同媒体多订阅共享 ledger，洗版/facts 互相影响

- **严重度**：中
- **涉及文件/函数**：`crates/store/src/schema/tables.rs`（subscribes）；`crates/store/src/subscribe_library.rs` `library_subscribe_facts`；`crates/api/src/http/subscriptions/create.rs`
- **根因**：无 (media_id, library, coverage) 唯一约束；facts 合并整个媒体的 ledger。
- **具体改法**：
  1. 创建/修改订阅时校验同库同季覆盖范围不重叠（返回 409 并提示合并）。
  2. 洗版删除前校验被删路径不被其他订阅 facts 引用。
- **验收标准**：
  - 集成：重复创建返回 409；两个订阅引用同一文件时洗版不删除。
- **流程测试场景**：同一媒体同一库创建两个重叠订阅；以及两个订阅引用同一在库文件时触发洗版。
- **通过标准**：重复创建返回 409；洗版不删除被另一订阅引用的文件。
- **风险与回滚**：风险：已有重复订阅需迁移提示。回滚：revert 校验。

### G-M3. 单季覆盖无自动跨季

- **严重度**：中
- **涉及文件/函数**：`crates/domain/src/lib.rs` `Coverage::Tv`；`crates/api/src/worker/transfer.rs`（完成后）；Catalog
- **根因**：订阅只覆盖单季，follow_future 不追新季。
- **具体改法**：
  新增选项 `auto_next_season`：季完成（M4a）且 Catalog 出现 S+1 时自动创建下一季订阅（复制规则组/库/下载器配置），并发通知。
- **验收标准**：
  - 集成：S01 完成且 Catalog 有 S02 → 新订阅创建一次（幂等）。
- **流程测试场景**：S01 订阅开启 auto_next_season，全流程完成 S01 入库；mock TMDB 新增 S02。
- **通过标准**：恰好自动创建 1 个 S02 订阅（重复运行不再创建）并发送通知；默认关闭时不创建。
- **风险与回滚**：风险：自动创建意外订阅——默认关闭。回滚：开关。

### G-M4. 整季包模式无包时不回退单集

- **严重度**：中
- **涉及文件/函数**：`crates/subscribe/src/choose.rs` `choose`（full_season_pack 分支）
- **根因**：找不到包直接返回空。
- **具体改法**：
  新增 `pack_fallback_after: Option<days>`：订阅创建或季末集播出后超过 N 天仍无包 → 回退按集选择（复用 M1 逻辑）；UI 说明。
- **验收标准**：
  - 单测：fallback 期限前无单集选择，期限后选择单集。
- **流程测试场景**：full_season_pack 订阅设置 pack_fallback_after=3 天，mock 索引站只有单集；时钟分别前进 1 天和 4 天。
- **通过标准**：1 天时不提交；4 天后提交单集并入库；未配置时保持只等整包。
- **风险与回滚**：风险：与“只要整包”的用户预期冲突——默认 None 保持原行为。回滚：配置。

### L1. 纯季包识别过宽

- **严重度**：低
- **涉及文件/函数**：`crates/subscribe/src/choose.rs` `is_pack`、`covered_slots`
- **根因**：只含 `S01` 无集号即视为整季包。
- **具体改法**：
  有 `size_bytes` 时要求体积 ≥ 单集平均体积 × 集数 × 0.5；Transfer 时按实际文件数核对，未覆盖的槽位释放占位。
- **验收标准**：
  - 单测：小体积 `S01` 资源不被视为整季包。
- **流程测试场景**：full_season_pack 订阅，mock 索引站返回体积很小的 `Show S01 SP`。
- **通过标准**：不被视为整季包，不占满槽位，不提交。
- **风险与回滚**：风险低。回滚：revert。

### L2. full_season_pack 只接受与范围完全一致的包（设计取舍）

- **严重度**：低
- **涉及文件/函数**：`crates/subscribe/src/choose.rs` `is_pack`；`domain` `Coverage::validate`
- **根因**：`release.episode==from && episode_to==to` 严格相等。
- **具体改法**：
  允许包范围 ⊇ 订阅范围（超出部分不入库）；UI 文案说明。
- **验收标准**：
  - 单测：订阅 E01-E12，包 E01-E13 被接受，仅入库 E01-E12。
- **流程测试场景**：订阅 E01-E12，mock 索引站返回包 E01-E13，下载完成后 Transfer。
- **通过标准**：包被接受，仅 E01-E12 入库，E13 不入库。
- **风险与回滚**：风险低。回滚：revert。

### L3. ladder 非法维度静默忽略；未知维度一律拒绝

- **严重度**：低
- **涉及文件/函数**：`crates/subscribe/src/choose.rs` `ladder_for`、`ladder_compare`；规则组保存接口
- **根因**：`_ => continue` 吞掉错误维度。
- **具体改法**：
  规则组保存时校验维度名（resolution/source/codec/hdr），非法返回 400；运行时遇未知维度打 error 日志一次。
- **验收标准**：
  - API 测试：`resolutions` 被拒。
- **流程测试场景**：通过 API 保存 ladder 为 `resolutions`，再保存合法 `resolution,hdr` 并跑洗版流程。
- **通过标准**：非法保存返回 400；合法配置下洗版按维度比较。
- **风险与回滚**：风险低。回滚：revert。

### L4. “订阅已完成”通知标记永不重置

- **严重度**：低
- **涉及文件/函数**：`crates/api/src/worker.rs` `notify_subscription_complete`；`crates/store/src/subscribes.rs` `delete_subscribe`
- **根因**：setting 键写入后不清理。
- **具体改法**：
  完成态由 M4a 的状态迁移驱动通知；状态回到 active 时删除标记；`delete_subscribe` 事务内删除 `subscribe.completed:{id}`。
- **验收标准**：
  - 集成：删除一集后重新补齐再次通知；删除订阅后 setting 不残留。
- **流程测试场景**：订阅完成 → 通知 → 删除一集 → 重新补齐；随后删除订阅。
- **通过标准**：两次完成各通知一次；删除订阅后 `subscribe.completed:*` setting 不存在。
- **风险与回滚**：风险低。回滚：revert。

### L5. 开放窗口 wanted 单元被误解析的被拒候选撑大

- **严重度**：低
- **涉及文件/函数**：`crates/api/src/open_coverage.rs` `units`；`crates/api/src/worker.rs` `record_wanted_round`
- **根因**：`observed` 取被拒候选集号，无上限校验。
- **具体改法**：
  `observed` 仅接受 ≤ max(已知最高集+3, Catalog 已播最大集) 的值。
- **验收标准**：
  - 单测：被拒候选 E999 不扩展 units。
- **流程测试场景**：开放订阅，mock 索引站返回被规则组拒绝的 `Show S01E999`。
- **通过标准**：wanted 单元不超过已知最高集+3。
- **风险与回滚**：风险低。回滚：revert。

### L6. 删除旧文件时持全局 Store 锁做文件 IO；清理不持订阅 guard（跨实例部分见 G-M5）

- **严重度**：低
- **涉及文件/函数**：`transfer.rs` `commit_transfer_outcome`；`run.rs` `persist`；`task_listing.rs` `prune_absent_pending`
- **根因**：`remove_file_and_ledger` 在 Store 锁内调用；清理不获取 guard。
- **具体改法**：
  由 M6 的待删除队列实现锁外删除；清理逻辑（H3）持 guard。
- **验收标准**：
  - 慢盘模拟下其它 API 不被长时间阻塞。
- **流程测试场景**：在慢速文件系统（删除时注入 sleep）下执行洗版删除，同时并发请求其它 API。
- **通过标准**：其它 API 响应时间不受删除阻塞（P95 < 基线+50ms）；清理逻辑持 guard。
- **风险与回滚**：风险低。回滚：随 M6/H3。

### D-L1. completed_files 要求整种 100%

- **严重度**：低
- **涉及文件/函数**：`qbit.rs` `completed_files`；`transmission.rs` `completed_files`
- **根因**：整种进度门控。
- **具体改法**：
  去掉整种进度判断，仅按单文件完成收集；确保 `file_identity` 对部分文件正确映射；pending 仅在全部文件完成后标 imported。
- **验收标准**：
  - 集成：季包 E01 完成而 E02 未完成 → E01 入库，pending 仍 active。
- **流程测试场景**：季包 E01-E02，mock qB 中 E01 文件进度 100%、E02 50%，运行 Transfer；随后 E02 完成。
- **通过标准**：第一轮 E01 入库、pending 仍 active；第二轮 E02 入库后 pending 变 imported。
- **风险与回滚**：风险：做种中的文件被硬链不受影响；Move 模式需禁止部分收集。回滚：revert。

### D-L2. 每次查询全量拉取 torrents/info

- **严重度**：低
- **涉及文件/函数**：`qbit.rs` `exact_existing_hash`、`owned_info`
- **根因**：每个 pending 两次全量列表请求。
- **具体改法**：
  按 `?tag=`/`?hashes=` 过滤；Transfer 一轮内缓存快照（`RoutedDownloader` 级缓存）。
- **验收标准**：
  - mock 计数：N 个 pending 时 info 请求次数 ≤ 下载器数量 + N 次 files 请求。
- **流程测试场景**：mock qB 预置 2000 个任务，10 个 pending，运行一轮 Transfer。
- **通过标准**：`/torrents/info` 请求次数 ≤ 1（或带过滤参数），files 请求 ≤ 10；耗时满足基线。
- **风险与回滚**：风险低。回滚：revert。

### D-L3. names_match/torrent_matches_snapshot 匹配宽松（仅展示路径）

- **严重度**：低
- **涉及文件/函数**：`crates/downloader/src/lib.rs` `names_match`；`identity.rs` `torrent_matches_snapshot`
- **根因**：分词半数重合即匹配。
- **具体改法**：
  保持破坏性路径禁用；展示结果标注“推断匹配”；为该函数加 `#[doc]` 警示与调用点审计测试。
- **验收标准**：
  - 代码审计：破坏性路径无调用（grep 测试）。
- **流程测试场景**：构造与 pending 标题相近的他人任务，执行订阅清理/删除。
- **通过标准**：破坏性操作不作用于他人任务；展示中标注为推断匹配。
- **风险与回滚**：无风险。

### D-L4. 下载器被禁用/删除后的路由与旧 pending 处理

- **严重度**：低
- **涉及文件/函数**：`crates/api/src/delivery.rs` `enabled_id`、`client_for_id`；下载器删除接口
- **根因**：旧 pending 指向已删下载器时每轮报错且不清理；`downloader_id=None` 跟随默认变化。
- **具体改法**：
  删除下载器时提示并迁移/清理其 pending；提交时总是持久化具体 downloader_id；路由回退记录到通知。
- **验收标准**：
  - 集成：删除下载器后其 pending 被标记 orphan，Transfer 不再报错。
- **流程测试场景**：订阅 pending 指向的下载器被删除，运行 Transfer 与任务列表。
- **通过标准**：pending 被标记 orphan，Transfer 不再每轮报错；UI 给出迁移提示。
- **风险与回滚**：风险低。回滚：revert。

### G-L1. 完成通知使用 `load_subscribe_facts`，与库作用域 facts 不一致

- **严重度**：低
- **涉及文件/函数**：`crates/api/src/worker.rs` `notify_subscription_complete`
- **根因**：通知路径未用 `load_library_subscribe_facts`。
- **具体改法**：
  改用 `load_library_subscribe_facts`，并并入 M4a 状态迁移。
- **验收标准**：
  - 单测：facts 不在目标库时不发完成通知。
- **流程测试场景**：订阅 facts 中路径不属于目标库（库已切换），运行完成通知判断。
- **通过标准**：不发送完成通知；切换回正确库并补齐后发送一次。
- **风险与回滚**：风险低。回滚：revert。


### F-1. 季包只缺部分集时可选只下载所需文件（选择性下载）

- **严重度**：功能（新增，默认关闭）
- **涉及文件/函数**：`crates/downloader/src/lib.rs` `Downloader::select_files`（新增，默认不支持）；`qbit.rs`（`torrents/files` + `torrents/filePrio` priority=0）；`transmission.rs`（`torrent-set` `files-unwanted`）；`crates/api/src/delivery.rs` `RoutedDownloader`；`crates/api/src/worker/finish_search.rs`（提交后按缺集筛文件）；设置 `subscribe.selective_files`
- **根因**：选中季包仅为补齐少数缺集时，整包下载浪费带宽/磁盘，且已有集的文件在 M2 下会被跳过入库。
- **具体改法**：
  1. 新增设置 `subscribe.selective_files`（`true`/`false`，默认 `false`）。
  2. 订阅搜索提交季包后，若开启：按包的 slot 与本轮前的 facts（库内 + 在途）计算“缺失 slot”；缺失 slot 非空且少于包内全部 slot 时，逐文件用 `release::parse` 解析集号，属于已有 slot 的文件设为不下载；无法解析集号的文件（nfo、sample 以外的附属文件等）保留。
  3. qB：`POST torrents/filePrio`（`hash`、`id=i|j|…`、`priority=0`）；Transmission：`torrent-set {"ids":[id],"files-unwanted":[…]}`。
  4. 完成判定：qB 忽略 priority=0 的文件，所有想要的文件 progress=1 即视为完成；Transmission 沿用 `percentDone`（按 sizeWhenDone 计算）与 `fileStats.wanted`。pending/coverage/import 由此把种子视为已完成。
  5. 元数据未就绪（磁力无文件列表）时不筛选，记 info 日志，整包下载（不阻塞）。
- **验收标准**：
  - 关闭时行为不变（不发 filePrio / files-unwanted）。
  - 开启时只有缺失集被下载；所需文件完成即入库、pending 变 imported。
- **流程测试场景**：库中已有 S01E01–E08，订阅 S01 共 10 集；搜索只返回 S01 全季包（10 个文件）。分别在 mock qB 与 mock Transmission（rpc≥16）上：开启 `subscribe.selective_files` → 提交 → 断言 mock 收到 E01–E08 的 priority=0 / files-unwanted → mock 仅完成 E09、E10 → Transfer。另跑一组关闭设置的对照。
- **通过标准**：开启组 E09/E10 入库、E01–E08 不被重复导入、pending 为 imported；关闭组 mock 无文件优先级调用。
- **风险与回滚**：风险：文件名集号解析错误导致误跳过所需文件——仅在设置开启时生效，且无法解析的文件一律保留。回滚：关闭设置或 revert。

### G-H2b. 绝对集号映射为“季+集”（P3，承接 G-H2 推迟部分）

- **严重度**：中
- **涉及文件/函数**：`crates/subscribe/src/absolute.rs`（新增 `AbsoluteEpisodes`）；`RunInput.absolute`；`subscribe/src/add.rs`（准入后重映射）；`subscribe/src/collect.rs` `collect_one`（文件名重映射）；`crates/api/src/season_check.rs`（按目录构建映射，取代仅丢弃）
- **根因**：无季号标题（如 `Show - 13`，多见于动画）被解析为 S01E13；G-H2 只能在集号超过目录 S01 集数时丢弃，无法把它正确归到后续季。
- **具体改法**：
  1. 目录逐季读取集数，得到累计表；仅当存在“季号为推断且集号超出 S01 集数”的候选时才构建（惰性，最多 50 季）。
  2. 映射规则：季号为推断、集号区间整体落在同一后续季内 → 改写为该季的“季+集”，并清除 `season_inferred`；跨季或超出目录总集数 → 丢弃（保留 G-H2 行为）。S01 范围内的集号不变。
  3. 同一映射在 Transfer 收集时作用于包内文件名，保证文件入对的槽位。
  4. 搜索、手动执行、Transfer 三条路径都传入映射；目录不可用时退化为原 G-H2 行为。
- **验收标准**：订阅 S02 时，`Show Episode 13`（S01 共 12 集）被选中并入库为 S02E01。
- **流程测试场景**：FakeCatalog S01=12 集、S02=10 集；订阅 S02E01–E02；搜索返回 `The Long Watch Episode 13 1080p…`；提交 → mock qB 完成文件 `The Long Watch - 13 1080p.mkv` → Transfer。
- **通过标准**：pending 被选中、库内出现 S02E01；G-H2 原用例（S01 订阅不接收 13）仍通过。
- **风险与回滚**：风险：目录集数与站点编号习惯不一致（如 OVA 计入）会映射错位——仅影响无季号标题。回滚：revert。
