# 数据库、接口与资源修复总 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.
> 旧skill名不存在时使用本环境的等价执行技能和公开seam TDD，不安装未知技能。

**Goal:** 按独立可测的小任务修复本轮17项数据库/接口/内存资源发现，并提供真实的行为、规模、恢复与发布验收记录。

**Architecture:** 分数据库、接口、资源三个子项目，先正确生命周期和小范围SQL，再批量离锁/分页，再依赖失效/容量治理。共享schema/ApiState/Probe文件串行合并；现有分库/身份/Library边界/协议不变，不让性能优化削弱安全。

**Tech Stack:** Rust 2024、Axum 0.8、Tokio、parking_lot、rusqlite 0.37 bundled SQLite、React/TypeScript、pnpm/Node≥20。

**Spec:** [修复设计](<../specs/2026-10-15-performance-resource-remediation.md>)；[完整审查](<../../audits/2026-10-15-database-api-memory-review.md>)。

## Global Constraints

1. 用 [CONTEXT](<../../../CONTEXT.md>) 词汇；domain 无 IO；store/jobs/media-server 不依赖 api。
2. 保留四 Store 文件及独立 jobs/catalog/fanart 生命周期；不引入跨文件外键或假称跨文件原子事务。
3. 每个改动公开 seam RED→GREEN；每个触及 crate `cargo test -p <crate>`，请求结束稳定树 `cargo test --workspace`；真实失败单独记录，不编造绿色基线。
4. `.rs` 文件≤800 行，生产函数≤120 行，测试函数≤120 行；接近上限的文件先按责任拆分，不调大阈值。
5. 静态图片仍 public；Playback 流/字幕保持 Jellyfin 协议鉴权、即时撤销及 Library 可见性。
6. Store/Queue 不嵌套；Registry→Store 锁序保留；同步 DB/FS/CPU 工作进入有界 blocking；permit 存活到真实 worker 结束，timeout 只结束等待。
7. 所有离锁工作使用 owned 快照；执行副作用前短锁校验来源版本、配置、freeze/删除状态；strict 归属无默认回退。
8. 不访问生产 DB、不启动真实网络/Chromium/ffmpeg 做 cargo 测试；临时库、fake Catalog/ImageFetcher/FileRead/Capture/进程 runner。
9. 不默认删除过期详情 stale cache、不删除 active 工作/模型引用样本/用户观看事实；新增 retention 默认 `enabled=false,dry_run=true`。
10. 不记录 token、完整 URL、payload、观看明细；结构化日志记录容量、耗时、数量和错误类型。
11. SQL 性能测试用 bundled SQLite VM/progress/EQP；时间/RSS只做 release 测量，不以偶发毫秒门禁替代行为与工作量断言。
12. 不创建 GitHub issue；只提交本任务文件，用户未提交内容由其负责人管理。

## Review Focus

1. root/scope/角色在慢IO期间变化：所有只读结果符合当前授权，副作用不沿旧快照执行（A1/A3/A4）。
2. 请求取消不等于worker结束：资源真实并发、单飞、释放有证明（R1/R2/R3）。
3. SQL LIMIT/响应分页不等于扫描有界：VM/重facts解码和cold/hot分别验（D1/A3/A4）。
4. crash/restart/full queue/关闭清理：无任务丢失、引用误删或虚假完成（D3/D4/R3/R4）。
5. 文档/单测/合成跑分不等于生产和设备验收：未运行明确Not run，失败不得省略（X1）。

---

## 1. 文档导航和状态

| 文档 | 用途 | 当前状态 |
|---|---|---|
| [审查报告](<../../audits/2026-10-15-database-api-memory-review.md>) | 17项发现、证据、排除项、36项定向绿/全仓失败记录 | 已写文档，不是修复完成 |
| [修复设计](<../specs/2026-10-15-performance-resource-remediation.md>) | 兼容/安全约束、固定容量、失效/retention方案 | 待review，不授权生产清理 |
| [数据库 D1–D5](<2026-10-15-database-remediation.md>) | 查询、计数、投影、历史、默认归属设计门 | 未实施 |
| [接口 A1–A6](<2026-10-15-api-performance-remediation.md>) | 离锁、Jellyfin批读/分页、REST、统计、up-next | 未实施 |
| [资源 R1–R5](<2026-10-15-memory-resource-remediation.md>) | 海报、缓存、章节、Probe、signal | 未实施 |

共 **16个分领域任务（15个实施任务、1个设计门）+ X1验收任务**；D5是设计门，不冒充产品修复。每任务内都有Files/Interfaces/测试RED/GREEN/提交步骤。代码片段是关键断言/接口，不是可复制即完整的测试；执行者须在指定测试文件创建同任务fixture/helper，不能假设变量或fixture已存在。

与 [旧收敛计划](<2026-10-13-database-api-followup.md>) 的关系：旧T1–T9已有实现提交、验收未全完成，T10仍独立；本轮只处理新近源码重证的发现，不机械重跑旧unchecked，不覆盖历史实测。

## 2. 基线与开工门

- [ ] **B0：冻结本次实施基线。** `git status --short`、`git rev-parse HEAD`，记录dirty清单和负责人；若用户Probe/marker修改仍进行中，不更改其文件，待其合并/明确范围后开始R3/R4/D2。
- [ ] **B1：核对失败测试。** 本轮审查曾 `cargo test --workspace` 在API lib136过/1失败（临时跨Library marker测试None unwrap），单独复跑也失败。由该改动负责人判断预期/业务缺陷，记录新命令/结果；不删测试、不用--skip冒充全绿、不夹带修复到文档任务。
- [ ] **B2：确认执行模式与破坏性边界。** 用户review本设计/子计划；新增retention默认关闭+dry-run；真实启用、默认归属ADR/backfill、数据库类型/默认分页等另行授权。
- [ ] **B3：执行环境。** crates单独可测、无真实网络/Chromium/ffmpeg；Node≥20、现有pnpm工具链。若旧技能不可用走现有executing-plans，不安装新技能。

## 3. 建议顺序与依赖

```text
B0/B1/B2/B3
 ├─ R1 海报真实permit → R2 TTL/gate
 ├─ D1 SQL两热点
 ├─ R5 signal
 └─ A1 FS离锁+Subscribe快照+视图拆分
       ├─ A6 up-next本地优先
       └─ A2 Jellyfin批读 → A3 Jellyfin选页/NextUp

D1 → D3 durable依赖/投影head → A4 REST按页facts → A5统计复用
A1/R2 → R3章节有界持久派发 → R4 Probe有界恢复
B1 → D2 Probe计数（与R4串行）
D3/R3/R4稳定 → D4 opt-in分类retention
D1/D3/A4实测 → D5默认归属设计门（用户批准前停在设计）
以上适用任务 → X1行为/规模/恢复/设备与发布记录
```

D1/R1/R5可独立研究/验证；并行实施仅在用户选择且文件写域确实分离时。序列图表示推荐合并顺序，不允许共享文件多人同时覆盖。A5可先交“一次聚合”子切片，全部关闭API-03须统计热路径实测。A2先批读不能提前关闭API-01，A3后再关。

## 4. 文件共享与提交门

| 写域 | 会修改的任务 | 合并规则 |
|---|---|---|
| Store schema/lib/projection | D1/D2/D3/D4/R3/R4/A4 | 单一串行迁移链，每提交从最新schema递增，重读最终diff |
| ApiState/lib/Probe | R3/R4、A1部分、用户现有修改 | 先协调用户基线；R3后R4；无guard跨await |
| poster service | R1/R2 | 严格R1后R2；身份/permit协议不可分叉 |
| library handler/selection | A1/A4/A5 | A1拆分后按页再统计；保持JSON/可见性 |
| playback_views | A1/A6 | A1先按责任拆分，再A6；不挤过800行 |
| media-server trait/routes | A2/A3 | A2保持trait；A3新增typed query和mock兼容 |
| maintenance/jobs kind/UI | D4 | 独立切片，全match/严格payload/UI一起验 |
| web HTTP | R5 | 独立，只抽signal生命周期 |

每个任务结束：自己的test RED/GREEN→每触及crate全测→diff与大小检查→只stage明确任务文件→小提交。执行者不得stage整个工作树；有重叠用户改动的文件只提交自己明确隔离的hunk，不能证明则等负责人合并。最终稳定树workspace一次，由Lead统一，不并行cargo/formatter修改。

## 5. Task X1：验收矩阵与真实发布记录

**Files:** Create `crates/api/tests/remediation_acceptance.rs`、`crates/api/tests/remediation_scale.rs`、`docs/benchmarks/2026-10-15-performance-resource-acceptance.md`；Modify [审查状态表](<../../audits/2026-10-15-database-api-memory-review.md>)只添加每ID关闭证据，不覆写原实验。测试support新建 `crates/api/tests/support/remediation.rs`，以fake/temp组合前面各公开seam。

**Interfaces:** 消费D3 ProjectionHead、A3 ServerItemPage、A4页facts、R1真实fetch容量、R3/R4任务统计与drain。新增仅test measurement `ScaleSample { median_ms:f64,p95_ms:f64,response_bytes:usize,vm_steps:u64,decoded_rows:usize,peak_rss_bytes:Option<u64>,store_lock_wait_ms:f64,store_lock_hold_ms:f64,threads:usize,children:usize,db_bytes:u64,wal_bytes:u64 }`；无采样能力的字段写None/Not run，不填0假称已测。

- [ ] **Step 1：写组合失败测试。** `slow_io_and_maintenance_keep_auth_and_heartbeat_available`、`cancel_and_restart_do_not_expand_worker_budgets_or_lose_jobs`、`scoped_pages_remain_equivalent_under_background_changes`。混合20库/多拷贝/隐藏库、200章节、Probe满窗口、poster取消、维护关闭：

```rust
assert_eq!(visible_ids, expected_authorized_ids);
assert!(actual_poster_fetches <= 4);
assert!(actual_chapter_processes <= 2);
assert!(metadata_pending <= 64 && fingerprint_pending <= 16);
assert_eq!(lost_durable_work, 0);
assert_eq!(deleted_after_disable_boundary, first_committed_batch);
```

各gate在assert前有RAII放行，整个测试失败也不能残留无限worker。组合行为红是旧实现基线能复现的问题，实施完才补写时标为新增绿色回归，不编造RED。
- [ ] **Step 2：跑RED/表征并保留记录。** `cargo test -p api --test remediation_acceptance -- --nocapture`。对未实施任务是行为红；已实施任务是组合表征，不把二者混在日志。
- [ ] **Step 3：只做验收所需接线。** 使用前面task的公共接口，不在验收任务偷偷修新业务。发现缺陷退回所属任务；新需求另记录，不扩展成数据库迁移/播放器重写。
- [ ] **Step 4：跑全部行为门。** 每触及crate `cargo test -p <crate>`，然后稳定树 `cargo test --workspace`；前端 `pnpm -C web test`、`pnpm -C web typecheck`、`pnpm -C web build`。记录完整命令/exit和未运行项；基线失败未解除就不能标整体验收通过。
- [ ] **Step 5：release规模矩阵。** `cargo test -p api --release --test remediation_scale -- --ignored --nocapture`；固定seed，100/2k/10k Media、1/5/20库、50k ledger、100k pending（恢复单项），冷/热/mixed/NFO对称、每格≥5轮，同DTO比较。D1 bundled1k→50k VM≤2倍；D2整轮100→1k VM≤15倍；A3 NextUp不按S次全读；A4热页重facts仅本页；R1/R3/R4容量不越界。时间/RSS目标先测基线再写实数，没有硬造30%/10ms收益承诺。
- [ ] **Step 6：恢复与人工设备门。** 用fake验证crash前后cursor、metadata→fingerprint、chapter running、清理disabled、old source publication。真实NAS/ffmpeg、VidHub/Infuse仅用户提供环境并授权后执行，记录版本/脱敏路由/Range206/416/断开和图片展示；没有设备填Not run，不以router mock代替。
- [ ] **Step 7：更新关闭矩阵并提交。** 每ID状态只能为 `open / partial / fixed-tested / design-gated / not-reproduced`，列具体代码提交、公开回归、规模证据；DB-07未批准保持design-gated。`git commit -m "test: record performance and resource remediation acceptance"`。独立全分支review后才交发布建议。

## 6. 回滚与上线

- 索引/派生列迁移仅新增，先支持版本预检；旧二进制拒较新schema，**不能把git revert当数据库降级方案**。上线前备份app/library/subscribe/jobs一致操作窗口，catalog可重建但离线stale价值需保留说明。
- R1/R2与R5可独立上线；章节/Probe保持DB待办可恢复，回滚前停止新派发并drain，不能删除queued恢复事实。
- D3 journal上线初始保守full rebuild，旧rev仍保护；若失效/页语义不符，切回现有有界全量读取并记录降级，不能绕过scope。fallback开关只调读路径，不删除新表/用户数据。
- D4默认关闭且dry-run，先人工review保护/候选数，启用需另确认；不设置默认VACUUM或自动详情删除。
- 默认分页/新权威root_id/连接池未获批准不实施。D5只提交设计不写生产。

## 7. 自审记录（本轮文档工作）

- [x] 覆盖17项：审查矩阵映射D/A/R，DB-03合并A1，DB-07独立设计门；无项静默丢弃。
- [x] 每个实施任务有精确文件、接口/类型、失败断言、命令、GREEN与提交；未知接口明确为计划新增。
- [x] 跨计划类型一致：ProjectionHead→A4、ServerItemQuery/Page→A3、ProbeDispatchStage不替代ProbeStage；Store不依赖API。
- [x] 五个Review Focus均落实到所属测试，安全与容量不是仅靠返回数量断言。
- [x] 本轮只写文档，未执行D/A/R/X1，不创建issue，不启用删除，不覆盖现有用户改动。
- [x] 文档结构/链接/17项ID覆盖与空白检查通过；文档阶段重跑 `cargo test --workspace` 完成exit 0。外部并行用户修改已更新跨库Probe用例，详见审查§7.3；不将该绿色结果归因本轮文档，也不据此关闭规模/泄漏项。

## 8. 执行交接

请先review本设计和三个子计划，再选择：

- **Subagent-driven（推荐）**：每任务由新上下文实施和独立review，再做整支review。适合这里跨schema/权限/取消恢复的高风险接口；共享写域仍串行。
- **Native**：由主agent按任务顺序实施，最后一次独立整支review，成本较低但中途独立复核较少。

**本轮没有执行授权。** 用户确认计划和方式后才开始B0与首任务；已有审查agent的创建不等于批准全部修复或生产维护。
