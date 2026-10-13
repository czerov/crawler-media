# 第二轮修复复审问题整改 Implementation Plan

> **历史状态（2026-10-13 整理）：** 第二轮功能/规范整改历史方案状态 UNRECONCILED；T1–T8 等原编号与风险接受记录保留，须逐 ID 核对实现、恢复及浏览器验收，不把草案时期的“未执行”当成当前能力结论。
>
> 本文保留原设计、发现/任务 ID 与当时证据；日期本身不表示过时。旧执行/提交/清理命令仅作历史记录，不应直接重跑；复选框不是当前实现清单。当前依据：[数据库/API review](<../../audits/2026-10-13-database-api-review.md>) 与 [剩余计划](<2026-10-13-database-api-followup.md>)，未覆盖的旧 ID 仍须单独对账。

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.
> 上述是计划模板中的执行模式名称；实际执行仅加载当前环境提供的对应 skill，不假定未安装的 skill 可用。团队执行须遵循本仓库的写入范围与依赖约束。

**Goal:** 修复第二轮复审确认的 10 项功能问题及 4 类规范问题，以故障恢复、文件身份、播放交付和策略往返的行为测试证明修复闭环。

**Architecture:** Library ledger 是已拥有文件身份和质量的真值，Subscribe facts 是其 coverage 投影；恢复必须按文件身份 reconcile，而不是按 Torrent 标题猜测。字幕轨道采用共享稳定索引和共享交付能力；设备撤销绑定认证会话而非信任可省略的请求 Header。文件恢复和 watcher 区分期望状态、执行状态及已确认成功状态。

**Tech Stack:** Rust workspace、SQLite/rusqlite、Axum、Tokio、React/TypeScript、Node 原生测试运行器。

## Global Constraints

- 当前代码快照：`e9e87dcf6c6df70d517981f4272405adddbc680c`；上一轮修复前基线：`142e0bb`。
- 本文是当时整改计划；上述历史代码快照包含所述缺陷，不能据此推定当前代码仍有全部同样缺陷。当前逐 ID 闭环状态未重核，见顶部 UNRECONCILED 边界。
- 问题来源：[原审计报告](<2026-10-05-full-project-bug-audit.md>)及当前会话第二次复审；复审大多数结论为源码/调用链确认，不冒充浏览器或生产环境复现。
- 每个 issue 是独立垂直 slice；执行前确认对应 issue、依赖和授权。不要将所有整改合成一个大修复提交。
- 遵循 [AGENTS.md](<../../../AGENTS.md>)、[CONTEXT.md](<../../../CONTEXT.md>)；不改动用户 [README.zh.md](<../../../README.zh.md>)。
- `.rs` 文件硬上限 800 行；生产函数及测试函数硬上限均为 120 行。按责任拆分，不通过删空行规避。
- `lib.rs` 只用于模块树与公共 re-export；domain 不新增 IO。
- 每个触及的 crate 单独执行 `cargo test -p <crate> --offline`，最终执行 `cargo test --workspace --offline`。
- HTTP/TMDB/Downloader/MediaProbe 使用注入 trait、fixtures 和 fakes。测试不访问真实 TMDB、Downloader 或启动 Chromium。
- 意外失败使用结构化 `error!`，可恢复降级使用 `warn!`，记录操作、对象及错误，默认不记录 token/API key；当前播放撤销日志的 token 例外见下方风险接受记录。
- 公共静态图片路由保持 public；字幕/视频遵循对应播放鉴权合同，不为解决取流问题公开字幕路由。

### 风险接受记录：当前播放撤销日志中的明文 token

- **确认时间与授权来源：** 2026-10-06（Asia/Shanghai），项目所有者在本轮会话明确要求：当前系统运行于自有服务器，明文 token 日志暂不整改，并将该决定写入文档。
- **适用范围：** 当前自有服务器部署中，`crates/media-server/src/routes/media/playback.rs` 已存在的视频/字幕撤销检查日志，包括显式 token 字段，以及 token 作为缺省 device_id 时被间接记录的情况。此项作为本轮整改的已接受风险，不再作为本轮验收阻断项；不表示漏洞已修复或 token 已脱敏。
- **规则例外：** 仅上述范围例外于本文及 AGENTS.md 的“不记录 token”要求；不是对其他模块、新增日志、API key 或其他凭据记录的通用授权。
- **残余风险：** 自有服务器不自动消除凭据泄露风险；能读取日志、日志备份或日志采集平台的人/程序可能获得可用会话 token。日志共享、故障排查导出、集中采集或服务器失陷仍可能扩大风险。
- **复审条件：** 部署环境不再符合当前自有服务器假设，或日志访问/分发范围扩大时，应重新评估并优先脱敏或移除凭据字段；对外分享日志前应移除 token。
- **不受此例外影响：** T7 的 token 与设备持久化绑定、设备身份一致性验证、设备撤销绕过修复和查库失败 fail-closed 仍是必须完成的功能要求。

---

## 问题与任务覆盖表

| 复审项 | 等级 | 当前缺陷 | 整改任务 |
|---|---|---|---|
| 1 / B03 | P1 | 季包 mapping 重试用整包 coverage，将多集指向一个文件 | T1 |
| 2 / B03 | P1 | 仅修复 absent facts，非空但失效的升级 facts 被保留 | T1 |
| 3 / D09 | P1 | keepOldFromSpec 没有序列化/反序列化数据来源 | T2 |
| 4 / B07 | P2 | 真冲突 return Err 丢弃整批成功结果 | T3 |
| 5 / B10 | P2 | hardlink/unlink 或复制中断留下双路径/部分目标，固定目标永远 AlreadyExists | T4 |
| 6 / D06 | P2 | UpgradeLadder 排序替代 cutoff 达标判断 | T5 |
| 7 / G07 | P2 | SRT 未转换 VTT、内封轨无抽取，planner 成功不等于可渲染 | T6 |
| 8 / G07 | P2 | Web/Jellyfin/handler 的外挂字幕索引不一致 | T6 |
| 9 / A05 | P2 | 省略 Device Header 即退回另一身份，原 token 仍能取流 | T7 |
| 10 / watcher | P2 | watch 失败仍纳入 currently_watched，不再重试 | T8 |
| 规范：函数长度 | 硬约束 | 修改后的函数仍超过 120 行 | T1、T3、T9 |
| 规范：错误日志 | 硬约束 | TMDB/字幕失败仅返回结果或 404 | T6、T9 |
| 规范：撤销查询 | 硬约束 | unwrap_or(false) 吞掉失败并放行 | T7 |
| 规范：schema 失败 | 硬约束 | schema 查询和 ALTER 失败被吞掉 | T4 |
| 重复代码 | 判断性 | 弹窗两个分支重复计算保留策略 | T2 |

## 接口与写入范围地图

以下新接口是拟议边界，执行时先依据现有类型核对；不得只加入抽象而不实现行为。已有函数应在调用链允许时保持兼容 wrapper。

- T1：`subscribe::collection_destinations` 携带每个 source 的 ledger 行、已确认 slot 集合和质量；`collect` 对其做 facts reconcile。API worker 从持久化 ledger/source mapping 构造它，不从 Torrent 标题推导整个季的归属。
- T2：Filter 的保留策略作为显式持久化字段，不混入评分 atom。Create/Patch Subscribe 中明确值优先于 Filter 缺省值；运行时唯一删除开关仍为 `Subscribe.keep_old_versions`。
- T3：WatchOutcome 增加逐文件失败结果；成功列表与冲突列表同时返回。单文件错误不丢弃已有成功结果。
- T4：recycle recovery 持久化恢复目标和阶段，并用原子 no-replace 完成目标发布；完整目标和复制临时文件必须可区分。
- T5：独立 `target_reached(owned: &Release, target: &Release) -> bool`；只有目标显式声明的维度参与达标判断，全部满足才达标。
- T6：共享字幕轨索引 `subtitle_index(tracks: &library::Tracks, ordinal: usize) -> u32`；共享交付结果包含 bytes 与真实 Content-Type。Web/Jellyfin 调用同一实现。
- T7：认证层解析并绑定稳定设备身份；取流 handler 使用已认证设备上下文，禁止用缺省字符串替代已绑定身份。
- T8：watcher 配置快照与实际监听集合分离；失败保留重试资格。
- T9：metadata 凭据验证、proxy probe 配置及执行按责任分成 sibling modules，补离线失败测试。

共享字幕工具不放进 api（media-server 不能依赖 api）；适合放在 library 的字幕模块，并通过公共小接口供两个入口使用。

---

## Task T1：以文件 ledger 身份重建 Subscribe facts（复审 1、2）

**Files:**
- Modify: [collect.rs](<../../../crates/subscribe/src/collect.rs#L58-L83>)、[collection_destinations.rs](<../../../crates/subscribe/src/collection_destinations.rs>)、[transfer.rs](<../../../crates/api/src/worker/transfer.rs#L25-L43>)。
- Inspect: [file_identity.rs](<../../../crates/subscribe/src/file_identity.rs>)、[facts.rs](<../../../crates/subscribe/src/facts.rs>)、[delivery.rs](<../../../crates/api/src/delivery.rs>)及 ledger/source mapping 的 store 实现。
- Test: 新建 `crates/subscribe/tests/fact_reconciliation.rs`；API 故障注入场景放 `crates/api/tests/management/transfer_retry/` 并在现有测试树注册。

**Interfaces:** API 提供每条 mapping 的 source、destination、Media/season/episode、已确认 coverage 及质量。collect 输出修正后的事实和待清理旧引用，不在恢复过程中删除仍被其他 slot 引用的文件。

- [ ] 写季包重试红灯：两条映射分别对应 E01/E02，facts 为空，断言 E01→dest1、E02→dest2；仅存在 E01 mapping 时 E02 必须保持 missing。
- [ ] 写升级中断红灯：facts 指 old（文件已删除），new ledger 已存在；重试后断言 facts 指 new，且不存在 imported-but-stale 状态。
- [ ] 写安全反例：更好的仍在位版本不得被低质量 pending 覆盖；同分经 ladder 批准的 replacement 必须可恢复；用户主动删除的目标不能自动重建。
- [ ] 运行 `cargo test -p subscribe --offline --test fact_reconciliation`，确认新场景失败且失败原因对应身份/路径错误。
- [ ] 增加 mapping 的权威身份字段；合并集文件须保存实际文件 coverage，不能用整包 release 补足未知 slots。对未知身份 fail closed，记录错误且不 mark imported。
- [ ] 按下面顺序实现 reconcile，并拆到独立 helper：

```text
for each persisted owned file:
    validate Media identity and actual covered slots
    intersect file slots with Subscribe coverage
    for each slot:
        absent fact -> restore authoritative file fact
        fact.path == this file -> refresh authoritative quality
        stale fact pointing at missing old file -> reconcile approved replacement
        different still-present better file -> preserve existing fact
mark pending imported only after its covered facts are confirmed and persisted
```

- [ ] 复核 Move 源消失后的恢复不能只依赖 Downloader 再列出视频；恢复应可从持久化 journal/ledger mapping 驱动。
- [ ] 故障注入覆盖 new ledger 提交后、facts 提交前中断；旧文件物理删除不得先于可恢复 replacement 记录的持久化。跨数据库不宣称一个普通事务就解决全部崩溃一致性。
- [ ] 跑 `cargo test -p subscribe --offline` 和 `cargo test -p api --offline`，review 删除安全和恢复时序后独立提交。

**Acceptance:** 季包无伪造事实；陈旧非空事实被正确修复；更好在位版本和用户删除语义不退化；恢复成功前不得标记 imported。

## Task T2：保留旧版本策略完整往返（复审 3）

**Files:**
- Modify: [subscribe-dialog.tsx](<../../../web/components/subscribe-dialog.tsx#L240-L280>)、[subscriptions API](<../../../web/lib/api/subscriptions.ts>)、[rule-sets-panel.tsx](<../../../web/components/rule-sets-panel.tsx>)、[rule_sets.rs](<../../../crates/api/src/http/rule_sets.rs>)。
- Inspect/Modify as required: domain Filter、store Filter 的持久化实现、Create/Patch Subscribe handlers。
- Test: 新建 `web/lib/rule-retention.test.ts`；API 增加 rule persistence/Create/Patch Subscribe 行为测试。

**Interfaces:** 为 Filter/RuleSet 增加 `keep_old_versions: bool` 策略字段；缺省 false 以兼容现有记录。Subscribe 显式策略覆盖规则组默认值，明确 false 不得用 truthy 判断丢弃。

- [ ] 写红灯：规则面板保存 true，GET/重新打开仍为 true；创建 Subscribe 不传显式覆盖时继承 true，显式 false 时为 false。
- [ ] 写红灯：PATCH 规则组与已有 Subscribe 行为明确分开；默认不追溯修改已有 Subscribe，修改已有记录必须调用明确的 Subscribe 策略 PATCH。
- [ ] 执行新 Node/API 测试，确认序列化丢失场景失败。
- [ ] 把策略作为 RuleSet/Filter 显式字段持久化，不伪装成 resolution/title/score atom；旧数据 migration/default 为 false。
- [ ] 删除对 `ruleSetSpecFromAtoms(...).upgrade_keep_old` 的无效判断。UI 从 RuleSet 策略字段读写，后端创建 Subscribe 时计算：

```text
effective_keep_old = explicit_Subscribe_value.unwrap_or(selected_Filter.keep_old_versions)
```

- [ ] 在创建弹窗分支之前只计算一次策略；TV 与 Movie 共用，保留 false 的明确覆盖能力。
- [ ] 做一次真实 round-trip 行为测试，再跑 `node --test`、`tsc --noEmit`、相关 crate 测试，独立提交。

**Acceptance:** 保存/重开/创建/明确覆盖均可观察；不再存在始终为 false 的分支；已存在 Subscribe 的更新合同明确。

## Task T3：Watch 批次成功结果不因单文件失败丢失（复审 4）

**Files:**
- Modify: [watch.rs](<../../../crates/library/src/watch.rs#L215-L260>)、[watch_intake.rs](<../../../crates/api/src/watch_intake.rs#L50-L62>)及 WatchOutcome 的其他调用方。
- Test: [Watch 测试](<../../../crates/library/tests/watch.rs>)和 API Watch intake 集成测试。

**Interfaces:** WatchOutcome 同时携带 transferred、unidentified、逐文件 errors；API 先持久化成功结果，再将失败摘要返回 Job。目录整体无法枚举时可以整体失败。

- [ ] 新测试：批次含正常 A、同名不同内容冲突 B、正常 C；断言 B 原字节未改，A/C 有 ledger，冲突在结果中可见；不依赖 read_dir 的顺序。
- [ ] 新测试：连续两轮 hardlink intake 幂等；跨文件系统 copy 的重试不能仅靠 inode。inode 判断必须同时比较 dev+ino，Unix 专用逻辑须有平台保护。
- [ ] 确认测试在当前批次 `Err` 行为下失败。
- [ ] 将单文件错误收集到 outcome，保持下列顺序：

```text
scan each file -> transferred successes + per-file errors
persist successful ledger rows
enqueue successful probes
return/log failure summary without discarding successes
```

- [ ] hardlink 重试用已确认实体身份；copy 重试使用持久化 source→dest 映射与完整性验证，不能把任意同名目标当成功。
- [ ] 底层 Transfer/Scrape 单文件失败也适用部分成功合同；不能只特殊处理 AlreadyExists。
- [ ] 在按责任拆分时把单文件处理 helper 保持在 120 行以内；跑 library/API 测试后独立提交。

**Acceptance:** 冲突不覆盖旧字节、不阻塞其他文件、不丢已有成功账本；Job 仍如实报告错误。

## Task T4：recycle 恢复状态机与断点恢复（复审 5）

**Files:** Modify [legacy_recycle.rs](<../../../crates/store/src/legacy_recycle.rs>)；Test [恢复测试](<../../../crates/store/tests/legacy_recycle_recovery.rs>)，需要时拆分 fixtures/helpers。

**Interfaces:** 持久化目标、原始源身份、恢复阶段（planned/copied/published/registered）；临时复制路径与最终发布路径明确区分。schema 操作返回真实 StoreError，不吞掉错误。

- [ ] 写红灯：source/target 同时存在且为同一个 dev+ino，模拟 hardlink 后 unlink 前中断；reopen 后正确登记一次并完成清理。
- [ ] 写红灯：临时复制只完成部分字节，模拟跨盘中断；重启不把部分目标登记成成功，并能从 source 重新完成复制。
- [ ] 写红灯：已保存目标被另一文件占据；不覆盖、不认领该文件，保留可恢复记录并记录错误。
- [ ] 确认 current fixed-target AlreadyExists 测试失败。
- [ ] 实现恢复协议：planned 意图先持久化；copy 写专用临时文件，完整性确认后同步并 no-replace 发布；只有完整已发布目标可被注册。遇双路径先验证身份，再完成阶段，不无条件再次 hard_link。
- [ ] 对旧记录无 recovered_path 且 source 缺失的情况，不能仅以 original.exists 证明归属；没有身份依据时保留记录供人工恢复，明确兼容策略。
- [ ] ALTER/schema 查询失败直接传播并带上下文日志；只有已证实的列存在属于正常兼容分支。
- [ ] 注入 ledger 插入失败、目标发布失败和 source 清理失败，验证重试安全；跑 `cargo test -p store --offline` 后独立提交。

**Acceptance:** 每个恢复阶段可重启；部分字节不登记、别人的文件不覆盖/认领、失败不静默。

## Task T5：将 cutoff 与升级排序分离（复审 6）

**Files:** Modify [choose.rs](<../../../crates/subscribe/src/choose.rs#L106-L125>)；Test subscribe 质量升级行为测试。

**Interfaces:** `target_reached(owned: &Release, target: &Release) -> bool` 与 ladder_compare 独立；显式目标字段全部满足，未知 owned 字段视为未达标。目标值解析失败必须报告而不是把空 Release 当已达标。

- [ ] 红灯：目标2160p、ladder仅source、owned720p WEB-DL，2160p候选不能被 cutoff 挡住。
- [ ] 红灯：目标2160p Remux、owned2160p WEB-DL 未达标；owned2160p Remux 达标；只设 source 目标时不凭空要求 resolution。
- [ ] 执行测试确认当前 lexicographic cutoff 错误。
- [ ] 用显式目标维度实现达标：

```text
specified target dimensions must be nonempty
for each specified dimension:
    owned dimension is known and rank(owned) >= rank(target)
all true -> reached
```

- [ ] UpgradeLadder 只决定候选是否比 owned 更好，不用于替代 target_reached；低质量和同质量候选不因 cutoff 修改变成可替换。
- [ ] 跑 subscribe/API 策略测试后独立提交。

## Task T6：字幕稳定索引、转换、抽取与鉴权（复审 7、8）

**Files:**
- Modify: [Web playback handler](<../../../crates/api/src/http/playback.rs>)、[Jellyfin playback](<../../../crates/media-server/src/routes/media/playback.rs>)、[MediaStreams DTO](<../../../crates/media-server/src/dto/media_streams.rs>)、[字幕 planner](<../../../web/lib/player/subtitles.ts>)。
- Create: `crates/library/src/subtitles/` sibling modules（index/delivery/conversion/extraction，各自只承担一个职责），通过 library 公共接口导出。
- Test: library fixtures/fake extractor；API session和字幕读取、media-server protocol字幕请求、Node planner行为测试。

**Interfaces:** DTO 与 handler 共用稳定索引：保留已有真实 stream_index；外部轨为其分配高于所有已有视频/音频/字幕 stream_index 的唯一序号，按缓存列表确定顺序。缓存更新需整体一致，不能在各入口用不同 enumerate offset。

- [ ] 写红灯：一条视频+音轨+两条外部字幕，Web/Jellyfin发布的每个URL都返回对应字幕字节，不能全部返回第一条或404。
- [ ] 写红灯：SRT `00:00:01,000 --> 00:00:02,000` 请求 format=vtt，断言返回 `text/vtt`、`WEBVTT` header 与点号毫秒时间戳；ASS保留样式，不转成VTT。
- [ ] 写红灯：内封srt/ass/pgs经 fake extractor 返回确定 bytes；路径不存在/抽取失败不返回空成功，并产生带轨索引/ledger上下文的日志。
- [ ] 写红灯：普通成员无权Library返回404；没有cookie仅query token的合法播放请求能认证；撤销/失效token不能访问字幕。
- [ ] 运行新测试确认索引、转换、query鉴权失败。
- [ ] 统一索引生成和查找。禁止 handler 的 `stream_index.is_none() && index == 0` 特例。
- [ ] 交付层按请求格式转换文本轨；内封轨通过注入的 extractor 抽取，生产用 ffmpeg argv 不用 shell 插值，测试使用 fake；PGS保留正确二进制类型。未知格式不得默认声明vtt。
- [ ] Web member认证目前只接受 Bearer/cookie，而 session URL 用 api_key：为播放相关取流路由建立窄范围query-token认证，不为整个管理API扩大query鉴权。
- [ ] 将文件读取、抽取和响应体读取从 Store 锁及异步执行线程移出；只在锁内快照必要对象。失败日志不要泄漏凭据。
- [ ] 除 planner测试外断言真实HTTP响应格式；独立运行 library、api、media-server、Node测试后提交。

**Acceptance:** 两端发布的所有URL可交付正确轨；SRT请求VTT实际转换；内封轨真实交付；访问权限不放宽；无cookie播放客户端有明确认证合同。

## Task T7：设备撤销绑定认证会话（复审 9）

**Files:** Modify [media-server auth](<../../../crates/media-server/src/auth.rs>)、[provider.rs](<../../../crates/media-server/src/provider.rs>)、[stream handler](<../../../crates/media-server/src/routes/media/playback.rs#L75-L89>)、[API provider](<../../../crates/api/src/media_server_provider.rs>)及 store 会话设备关联；Test media-server/API鉴权和播放测试。

**Interfaces:** 稳定 DeviceId 在认证/注册阶段与token或会话关联；客户端Header/query/MediaBrowser授权参数用于一致性校验，不能用其任意覆盖绑定身份。PlaySessionId只标识单次播放，不形成设备身份。

- [ ] 写红灯：设备D登录后被撤销，以相同token发新流请求，分别省略Header、使用DeviceId query、改PlaySessionId，全部拒绝；设备E的合法token不受影响。
- [ ] 写红灯：DeviceId与会话绑定不一致时拒绝；缺少设备绑定的旧会话必须按明确迁移策略处理，不静默归为jellyfin-client绕过撤销。
- [ ] 写红灯：撤销查库失败，取流返回5xx/拒绝，不放行。
- [ ] 运行测试确认当前仅Header分支存在漏洞。
- [ ] 先确定登录/设备注册及旧token兼容合同再实现绑定；若不能可靠验证匿名设备身份，应撤销关联token并要求重新认证，不能以“客户端能改Header”作为撤销已完成的理由。
- [ ] 取流使用认证注入的设备上下文；lookup Err显式记录error并拒绝，删除 `.unwrap_or(false)`。字幕取流同样应用此合同。
- [ ] 检查Web video原生请求能携带认证上下文。STRM已经交出的第三方直链无法撤回的限制需明确告知，不伪称可即时阻断。
- [ ] 跑 media-server、store、api 离线测试后独立提交。

**Acceptance:** 被撤销设备原token新取流失败，省略/更换身份表达不绕过；其他设备不误伤；失败时fail closed。

## Task T8：watcher 实际状态与重试（复审 10）

**Files:** Modify [fs_watcher.rs](<../../../crates/api/src/fs_watcher.rs#L190-L205>)；Test 内部注入 fake watcher 的行为测试。

**Interfaces:** `desired` 表示配置，`active` 仅表示已成功注册。配置读取应返回 Result，完整快照失败不当作空配置卸载全部监听。

- [ ] 红灯：watch首次失败、第二次成功，断言第二轮重试且active最终包含目录；原来不存在的目录创建后也应被监听。
- [ ] 红灯：unwatch失败后仍记录active并继续重试；配置查询失败不卸载已有监听。
- [ ] 确认当前直接赋值currently_watched的测试失败。
- [ ] 实现：

```text
desired = load_complete_config_snapshot()?  // error: keep current active set
for path in desired - active:
    watch success -> active.insert(path)
    failure/missing dir -> do not insert; next refresh retries
for path in active - desired:
    unwatch success -> active.remove(path)
    failure -> preserve active; next refresh retries
```

- [ ] 根据操作结果记info/error/warn；fake watcher测试不得依赖真实OS通知时序。
- [ ] 跑 API 测试后独立提交。

## Task T9：错误观测与尺寸门禁，不把绿灯当闭环

**Files:** Modify [settings.rs](<../../../crates/api/src/http/settings.rs>)、[proxy_settings.rs](<../../../crates/api/src/http/proxy_settings.rs>)；T1/T3/T6/T7/T4分别在其写入范围负责collect、scan、字幕、撤销、schema的尺寸与日志。

**Interfaces:** metadata验证使用可注入、不经过Catalog缓存的实时HTTP seam；请求与响应体读取均在阻塞任务内完成。代理diagnose拆为配置、target构造和单目标probe，而非一个181行函数。

- [ ] 加 fake HTTP red tests：无效key、响应读取失败、非法JSON、timeout均明确失败；不访问真实TMDB，不能回退Catalog缓存。
- [ ] 给错误日志添加stage、source和对象上下文，API key及token绝不输出；错误文本返回前核实HTTP库是否会包含凭据URL并脱敏。
- [ ] 按责任提取diagnose targets和单probe执行helper；不手工重写抽取内容引入协议变化。
- [ ] 用 `wc -l` 及函数起止范围核实所有本次修改的生产/测试函数≤120行、文件≤800行；历史尺寸问题不能以“之前就超限”为验收通过理由。
- [ ] 如果同时承担首轮复审历史尺寸整改，单列后续独立slice处理原报告涉及的 discover/media/playback_views/Store playback/CLI测试；不能仅拆settings便声称全部历史违规解除。
- [ ] 跑API离线测试、相关端点合同测试，独立提交。

## 执行顺序与团队边界

- 建议顺序：T1→T3→T4；T2、T5、T8可在无写入重叠时并行；T6先统一字幕接口，T7依赖T6的交付路由；T9最后核实规范门禁。
- T1/T5共享choose或collect测试辅助设施时必须明确一个owner；T6/T7共享provider/auth/playback时按依赖执行，禁止两个agent并发修改同文件。
- 每项开始前读取shared task最新revision，记录write_scopes；任务完成后Lead检查diff与行为测试。失败退出留下的部分修改不得按完成合入。
- 本文不授权推送、关闭issue或覆盖用户文件；执行阶段按用户授权及仓库issue流程处理。

## 最终验收与交付

- [ ] 对覆盖表逐项展示新增失败测试及修复后通过结果；记录未覆盖的环境/协议限制。
- [ ] 每个修改crate的单独测试通过。
- [ ] `cargo test --workspace --offline` 通过。
- [ ] 在web目录执行 `node --test`、`./node_modules/.bin/tsc --noEmit` 通过。
- [ ] `git diff --check` 通过；review staged diff不包含用户README及无关修改。
- [ ] 浏览器实际字幕渲染、两个外挂字幕切换、无cookie鉴权及设备撤销需要额外人工/浏览器验收；未执行时明确写“未执行”，不能称端到端100%完成。
- [ ] 发布修复清单、每项commit、测试证据和剩余限制。只有10项功能问题及承诺的规范范围全部闭环，才能宣告本轮目标完成。
