# 文档入口与权威层级

更新：2026-10-16。ADR-0007 的 Browser 能力边界随 2026-10-16 决策更新（单一 CDP 开关、受管 Obscura、失败回退 HTTP），下表 0007 行与 [deploy](<deploy.md>)、[CONTEXT](<../CONTEXT.md>) 同步；其余内容与清理记录仍为 2026-10-15。本次盘点按“已完成计划不再留在工作树”的原则清理了 `docs/`：删除 51 份已实施／已替代的逐轮计划、设计与复现附件（详见文末）。**旧文档的日期、unchecked 框或历史绿测都不表示当前功能未实现或已验收。**

## 当前先读

1. 术语与用户请求：[CONTEXT](<../CONTEXT.md>)；工程约束：[AGENTS](<../AGENTS.md>)。
2. **本轮数据库／接口／内存审查：** [2026-10-15 三方向报告](<audits/2026-10-15-database-api-memory-review.md>)（17 项发现，含隔离实验和全仓测试失败记录；不是生产测量）。历史数据库／接口固定快照见 [10-13 总复核](<audits/2026-10-13-database-api-review.md>)。
3. **本轮详细修复计划：** [总执行顺序](<superpowers/plans/2026-10-15-performance-resource-remediation.md>)、[共享设计](<superpowers/specs/2026-10-15-performance-resource-remediation.md>)，下分数据库 D1–D5、接口 A1–A6、资源 R1–R5。**待 review，未实施，不授权生产清理。** [旧收敛计划](<superpowers/plans/2026-10-13-database-api-followup.md>) 保留 T1–T9 缺失验收／测量和 T10，不机械重跑已有实现、不自动关项。
4. **维护任务合同／已实现与偏差：** [DB8](<superpowers/plans/2026-10-12-db8-maintenance-retention.md>)。
5. API 约定：[self](<api-contracts/self.md>)；完整现行路由读 [后端](<../crates/api/src/http/mod.rs>) 与前端 API 模块，文档不复制动态路由表。
6. 部署与默认清理警告：[deploy](<deploy.md>)；UI／设备验收清单：[system regression](<testing/system-regression-cases.md>)，**清单不是执行结果**。
7. 最近一轮三批优化的验收结果：[batched performance acceptance](<audits/2026-10-13-batched-performance/acceptance.md>)。
8. **当前数据库全量字典：** [数据库设计与查询关系总览](<database/README.md>)，下分 6 个 DB 的全部表、字段、约束、索引；明确区分真实 JOIN、跨文件应用拼接与待评审设计。

## 文档保留规则

- ADR 记录决策与边界，错误现状注释应更正，历史背景显式标出；不按日期删除决策。
- benchmarks 记录协议或固定快照的测量，不是实时生产事实；结果和未测项一起引用。
- 历史计划一旦验收完成即从工作树移除，正文可由 `git show` 取回；后续只更新当前复核状态与收敛计划，不继续向历史文档追加“第 N 轮完成／仍未开始”清单。
- 老审计只有确认代码＋公开测试对应后才能关项；未核验叫**未逐 ID 对账**，不等于“仍坏”或“已修”。**收敛计划 T10 逐 ID 核验直接依赖的三份审计（09-29／10-05／10-10）必须留在树内**；其余历史记录（锁审计、复现附件、逐轮计划与设计）验收完成后移出工作树，正文由 Git 保留。
- 可删：已替代的重复状态／错误候选表、临时复制源、失败编译日志、会覆盖历史证据的复现脚本；先迁移有效结论、提交和测量边界，不删唯一未闭环需求。

## ADR（保留的设计决定）

| 文件 | 用途／边界 |
|---|---|
| [0001 Media identity](<adr/0001-media-identity.md>) | 身份与别名，不把外部数字当全局唯一 |
| [0002 Hooks](<adr/0002-hooks-not-plugin-runtime.md>) | 编译期第一方扩展，不引入第二运行时 |
| [0003 Site credentials](<adr/0003-plaintext-site-credentials.md>) | 有意的 SQLite 明文凭证边界，不等于可日志输出／进 Git |
| [0005 Jellyfin](<adr/0005-jellyfin-compatible-playback.md>) | 实际客户端协议子集；未实现 UserConfiguration 明确保留 |
| [0006 Indexer](<adr/0006-indexer-in-first-slice.md>) | 架构 rationale，不是当前外网验收结果 |
| [0007 Browser](<adr/0007-cdp-browser-opt-in.md>) | Browser 仅用于登录/签到；导航前回退 HTTP；受管 Obscura；每会话独立 BrowserContext；存量库增量升级（r9） |
| [0008 SQLite](<adr/0008-sqlite-system-of-record.md>) | 四 Store 生命周期分区＋jobs/fanart；root_id／路径与历史背景已更正 |
| [0009 Job](<adr/0009-job-queue.md>) | Interval／queued-running 约束／状态事实；不宣称专用单写线程或全局 2 runner 上限 |
| [0010 Password](<adr/0010-password-hashing.md>) | Argon2id；其他独立 credential 生命周期历史计划不得机械重跑 |
| [0011 Probe retry](<adr/0011-probe-stage-retry.md>) | 持久化阶段／重试决定 |
| [0012 Library removal](<adr/0012-library-removal.md>) | freeze/drain、fresh ledger、安全归属与保留 User 状态；优于旧 STRM 删除草案 |

## 测量证据（不是新增跑分）

| 文档 | 状态 |
|---|---|
| [Fingerprint protocol](<benchmarks/2026-10-08-fingerprint-sampling-protocol.md>) | 人工测量协议，不代表真实整季 STRM 已跑 |
| [API release before/after](<benchmarks/2026-10-12-api-endpoints.md>) | 历史数据保留；gallery 未达 30% 降延迟、NFO 不对称／小历史限制明确 |
| [Jobs query plans](<benchmarks/2026-10-12-jobs-query-plans.md>) | bundled SQLite 热查询历史证据，不覆盖所有批读 |
| [Media identity indexes](<benchmarks/2026-10-12-media-identity-indexes.md>) | 50k 非唯一索引历史结论；未测写成本／大小 |

旧代码注释仍指向的 [endpoint evidence 入口](<audits/2026-10-12-remediation-review-repro/endpoint-perf.md>) 与 [Jobs evidence 入口](<audits/2026-10-12-remediation-review-repro/jobs-query-plans.md>)只做重定向，不再存第二份结果，避免为文档搬迁修改 Rust 源码。

## 历史审计（未逐 ID 对账，收敛计划 T10 依赖）

| 文件 | 处理 |
|---|---|
| [09-29 full audit](<audits/2026-09-29-full-codebase-audit.md>) | 保留全部 ID；FK 旧判断已显式纠正，需逐 ID 代码／测试映射 |
| [10-05 full bug audit](<audits/2026-10-05-full-project-bug-audit.md>) | 原放于 `superpowers/plans/`；本轮移入 `audits/` 保留，A/B/D/G 等 ID 仍需逐项闭环核对 |
| [10-10 full review](<audits/2026-10-10-full-project-review.md>)／[followup](<audits/2026-10-10-review-followup-remediation.md>) | 有实现切片，不证明 25 项及更早全部关闭 |

## Agent 文档

[domain](<agents/domain.md>) 是领域文档导航；[issue tracker](<agents/issue-tracker.md>) 仅用户明确要求时使用；[roadmap](<agents/roadmap.md>) 是阶段档案，不是当前 API 规范，不要求每次任务全读。

## 本轮删除记录（2026-10-15）

共删除 51 份文件，全部为已实施／已替代的历史产物，正文由 Git 保留；另将 10-05 全项目 Bug 审计从 `superpowers/plans/` 移入 `audits/` 保留（见上表）。

1. **逐轮实施计划 37 份**：09-26 至 10-13 的 `superpowers/plans/*`，除仍在进行的 [收敛计划](<superpowers/plans/2026-10-13-database-api-followup.md>) 与被源码注释引用的 [DB8 合同](<superpowers/plans/2026-10-12-db8-maintenance-retention.md>) 外全部移除。其中 10-13 三批（data safety／correctness／performance）的 design＋plan 与总路线图 `2026-10-13-database-api-scan-master-plan.md` 已实施并由 [acceptance](<audits/2026-10-13-batched-performance/acceptance.md>) 验收；总路线图“三批业务代码未开始”的状态已过时。
2. **设计草案 2 份**：`superpowers/specs/*`（intro/outro 初稿已被 ADR-0012 与版本化采样替代；adaptive 设计对应实现已落地）。
3. **基线复现附件 10 份**：`audits/2026-10-10-full-project-review-repro/**`，是对固定基线 277de10 的失败复现拷贝，原文明确“不得当作已修复的回归测试”。
4. **其余 2 份**：10-13 batch-3 的 7 行 baseline 存根（已被 acceptance 取代）、10-10 锁审计（历史记录，非当前结论）。
