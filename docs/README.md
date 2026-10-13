# 文档入口与权威层级

更新：2026-10-13。本次盘点范围是整个 `docs/`（含历史计划、审计与复现附件），不是只整理DB8。**旧文档的日期、unchecked框或历史绿测都不自动表示当前功能未实现／全部已验收。**

## 当前先读

1. 术语与用户请求：[CONTEXT](<../CONTEXT.md>)；工程约束：[AGENTS](<../AGENTS.md>)。
2. **当前数据库／接口状态与问题：** [总复核](<audits/2026-10-13-database-api-review.md>)（固定代码快照723394e）。
3. **唯一后续修复／验收计划：** [收敛计划](<superpowers/plans/2026-10-13-database-api-followup.md>)（T1–T10尚未实施，不自动授权修改生产数据）。
4. **维护任务合同／已实现与偏差：** [DB8](<superpowers/plans/2026-10-12-db8-maintenance-retention.md>)。
5. API约定：[self](<api-contracts/self.md>)；完整现行路由读 [后端](<../crates/api/src/http/mod.rs>) 与前端API模块，文档不复制动态路由表。
6. 部署与默认清理警告：[deploy](<deploy.md>)；UI／设备验收清单：[system regression](<testing/system-regression-cases.md>)，**清单不是执行结果**。

## 文档保留规则

- ADR记录决策与边界，错误现状注释应更正，历史背景显式标出；不按日期删除决策。
- benchmarks记录协议或固定快照的测量，不是实时生产事实；结果和未测项一起引用。
- 历史计划保留独立需求、失败模式、任务ID与验收目标；顶部标“历史／未逐项对账”，不能按旧unchecked命令重复迁移。
- 老审计只有确认代码＋公开测试对应后才能关项；本轮API复核不覆盖全项目每条历史发现。未核验叫**未逐ID对账**，不等于“仍坏”或“已修”。
- 可删：已替代的重复状态／错误候选表、临时复制源／失败编译日志／会覆盖历史证据的复现脚本；先迁移有效结论、提交和测量边界，不删唯一未闭环需求。

## ADR（保留的设计决定）

| 文件 | 用途／边界 |
|---|---|
| [0001 Media identity](<adr/0001-media-identity.md>) | 身份与别名，不把外部数字当全局唯一 |
| [0002 Hooks](<adr/0002-hooks-not-plugin-runtime.md>) | 编译期第一方扩展，不引入第二运行时 |
| [0003 Site credentials](<adr/0003-plaintext-site-credentials.md>) | 有意的SQLite明文凭证边界，不等于可日志输出／进Git |
| [0005 Jellyfin](<adr/0005-jellyfin-compatible-playback.md>) | 实际客户端协议子集；未实现UserConfiguration明确保留 |
| [0006 Indexer](<adr/0006-indexer-in-first-slice.md>) | 架构 rationale，不是当前外网验收结果 |
| [0007 Browser](<adr/0007-cdp-browser-opt-in.md>) | external-CDP-only；端点配置≠可用性验证 |
| [0008 SQLite](<adr/0008-sqlite-system-of-record.md>) | 四Store生命周期分区＋jobs/fanart；root_id／路径与历史背景已更正 |
| [0009 Job](<adr/0009-job-queue.md>) | Interval／queued-running约束／状态事实；不宣称专用单写线程或全局2 runner上限 |
| [0010 Password](<adr/0010-password-hashing.md>) | Argon2id；其他独立credential生命周期历史计划不得机械重跑 |
| [0011 Probe retry](<adr/0011-probe-stage-retry.md>) | 持久化阶段／重试决定 |
| [0012 Library removal](<adr/0012-library-removal.md>) | freeze/drain、fresh ledger、安全归属与保留User状态；优于旧STRM删除草案 |

## 测量证据（不是新增跑分）

| 文档 | 状态 |
|---|---|
| [Fingerprint protocol](<benchmarks/2026-10-08-fingerprint-sampling-protocol.md>) | 人工测量协议，不代表真实整季STRM已跑 |
| [API release before/after](<benchmarks/2026-10-12-api-endpoints.md>) | 历史数据保留；gallery未达30%降延迟、NFO不对称／小历史限制明确 |
| [Jobs query plans](<benchmarks/2026-10-12-jobs-query-plans.md>) | bundledSQLite热查询历史证据，不覆盖所有批读 |
| [Media identity indexes](<benchmarks/2026-10-12-media-identity-indexes.md>) | 50k非唯一索引历史结论；未测写成本／大小 |

旧代码注释仍指向的 [endpoint evidence入口](<audits/2026-10-12-remediation-review-repro/endpoint-perf.md>) 与 [Jobs evidence入口](<audits/2026-10-12-remediation-review-repro/jobs-query-plans.md>)只做重定向，不再存第二份结果，避免为文档搬迁修改Rust源码。

## 独立功能设计／验收（保留，不并入DB8）

| 文档 | 现状 |
|---|---|
| [初始intro/outro设计](<superpowers/specs/2026-09-22-intro-outro-detection-spec.md>) | 历史初稿；无条件STRM重用等以新来源版本／fresh-ledger规则为准 |
| [Adaptive design](<superpowers/specs/2026-10-08-adaptive-fingerprint-sampling-design.md>)、[实施计划](<superpowers/plans/2026-10-08-adaptive-fingerprint-sampling.md>) | 已有流水线提交，不再写未实现；真实整季采样／边界／流量证据未全确认 |
| [Incremental probe/retry](<superpowers/plans/2026-10-10-incremental-probe-and-fingerprint-retry.md>) | 已实现多轮；剩余恢复／URL变更验收独立 |
| [Library deletion/NFO reimport](<superpowers/plans/2026-10-10-library-deletion-and-nfo-reimport.md>) | 已实现＋硬化；不能把设计自审复选框当实机结果 |
| [Rename grace](<superpowers/plans/2026-10-11-rename-grace-sidecar-isolation.md>) | **原用户独立计划原样保留**；949c24c/876e7aa/3e7e9d2/93ea3bd已有实现，状态需逐task验收对账，未被API收敛删除或重写 |
| [TV season/NFO](<superpowers/plans/2026-10-10-tv-season-nfo-and-local-stills.md>)、[Scrape/artwork/Fanart](<superpowers/plans/2026-10-10-scrape-artwork-and-fanart.md>) | 前者较窄历史方案，后者已有实现；空Fanart key走内置fallback不是关闭 |
| [Embedded subtitles](<superpowers/plans/2026-10-07-embedded-subtitles-extraction-and-logging.md>) | 历史实现；受控交付／版本缓存以当前代码＋总复核为准 |
| [STRM directory deletion](<superpowers/plans/2026-10-07-strm-directory-deletion.md>)、[Deletion/scan/probe correction](<superpowers/plans/2026-10-09-fix-deletion-scan-and-probe-caching.md>) | 历史初稿与修订；旧清共享历史指令已标废弃，不能重执行 |

## 历史广域审计与锁记录（未逐ID关闭）

| 文件 | 处理 |
|---|---|
| [09-29 full audit](<audits/2026-09-29-full-codebase-audit.md>)／[remediation](<superpowers/plans/2026-09-29-full-codebase-audit-remediation.md>) | 保留全部ID；FK旧判断已显式纠正，需逐ID代码／测试映射 |
| [10-05 full bug audit](<superpowers/plans/2026-10-05-full-project-bug-audit.md>) | 历史审计放在plans的旧位置保留，避免丢引用；不是当前生产bug全表 |
| [10-10 full review](<audits/2026-10-10-full-project-review.md>)／[remediation](<superpowers/plans/2026-10-10-full-project-review-remediation.md>)／[followup](<audits/2026-10-10-review-followup-remediation.md>) | 有实现切片，不证明25项及更早全部关闭 |
| [10-10 repro](<audits/2026-10-10-full-project-review-repro/README.md>) | 保留独立广域证据，固定baseline运行；坏行为green≠已修复 |
| [Lock audit](<agents/lock-audit-2026-10-10.md>) | 历史锁／阻塞记录，物理删除离锁与相关安全门仍有效 |

## 其余历史实施记录（原ID／正文保留，状态不再误当待执行）

| 文件 | 归类 |
|---|---|
| [09-26 API correctness](<superpowers/plans/2026-09-26-api-crate-correctness.md>) | 历史／独立部分未逐项对账 |
| [09-26 UI followup](<superpowers/plans/2026-09-26-ui-correctness-followup.md>) | 历史UI切片，非当前浏览器验收 |
| [09-26 v1 unification](<superpowers/plans/2026-09-26-unify-v1-drop-legacy.md>) | 已有统一v1实现，历史步骤不重跑 |
| [09-26 envelope](<superpowers/plans/2026-09-26-v1-envelope-followup.md>) | 历史响应／旧尺寸规则已标注 |
| [09-27 discover failures](<superpowers/plans/2026-09-27-discover-section-failure-visibility.md>) | 历史需求，需对账独立剩余 |
| [09-27 discover hero](<superpowers/plans/2026-09-27-discover-hero-retry-and-empty-success.md>) | 历史followup，不由API绿测自动关 |
| [09-27 store boundary](<superpowers/plans/2026-09-27-store-boundary-and-size-review-fixes.md>) | 历史模块／尺寸记录 |
| [09-27 remaining sizes](<superpowers/plans/2026-09-27-remaining-size-limits.md>) | 原60硬限不再有效；当前120规则按AGENTS |
| [09-27 extract Store](<superpowers/plans/2026-09-27-extract-store-crate.md>) | 已有独立Store，历史迁移不重跑 |
| [09-27 password review](<superpowers/plans/2026-09-27-password-auth-review-fixes.md>) | 历史安全设计，遵循现行ADR0010 |
| [09-27 bootstrap gate](<superpowers/plans/2026-09-27-bootstrap-admin-password-gate.md>) | 历史启动凭证门，不能认为env每次覆盖 |
| [09-28 credential lifecycle](<superpowers/plans/2026-09-28-admin-cli-credential-lifecycle.md>) | CLI／admin独立生命周期背景 |
| [09-28 discovery refresh](<superpowers/plans/2026-09-28-full-discovery-listing-cache-refresh.md>) | 独立refresh契约历史，需对账 |
| [09-28 downloader identity](<superpowers/plans/2026-09-28-downloader-identity-and-transmission-safety.md>) | 独立identity／Transmission安全背景 |
| [09-28 project scan](<superpowers/plans/2026-09-28-project-scan-followup-fixes.md>) | 广域未逐ID对账 |
| [10-02 review remediation](<superpowers/plans/2026-10-02-review-remediation.md>) | 广域未逐ID对账 |
| [10-03 page/API module](<superpowers/plans/2026-10-03-page-api-module-remediation.md>) | 历史模块切片，保留独立非目标 |
| [10-05 followup](<superpowers/plans/2026-10-05-review-follow-up.md>) | 历史实施与未核验行政／验收步骤 |
| [10-05 second review](<superpowers/plans/2026-10-05-second-review-remediation.md>) | 历史目标；token日志旧风险不再可接受例外 |
| [10-06 latency/posters](<superpowers/plans/2026-10-06-api-latency-and-async-posters.md>) | 独立后台海报／浏览器测量目标，不等同C5b |

## Agent 文档

[domain](<agents/domain.md>)是领域文档导航；[issue tracker](<agents/issue-tracker.md>)仅用户明确要求时使用；[roadmap](<agents/roadmap.md>)是阶段档案，不是当前API规范，不要求每次任务全读。

## 本轮删除／替代记录

1. 删除10-11 API初审和复核、10-12追加backlog、10-11 master＋A/B/C四计划：当前有效交付／剩余项／误报纠正已迁入总复核＋T1–T10。旧“P0全进程停摆／root_id直接JOIN／B/C未开始”等不再作为有效说明保留。
2. 删除这两轮临时repro中的复制源、插桩、重复编译日志和生成脚本。R1/R2/R3已修，当前回归在crates；生成脚本会从当前源覆盖旧快照，不再推荐运行。保留有效性能证据并迁入benchmarks，旧源码引用保留两个短入口。
3. 10-10广域repro、所有ADR、独立rename计划、尚未逐ID对账的早期审计**未删**。
4. 因部分旧文件原本untracked（未进Git），删除前另有带SHA256清单的本机临时备份 `/tmp/crawler-media-doc-consolidation-20261013/`。它只用于本次撤销，不是仓库文档或可长期依赖证据。已跟踪历史可由Git恢复。

后续新增修复只更新当前复核状态与后续计划，不继续向历史文档追加第N轮“完成／仍未开始”清单。
