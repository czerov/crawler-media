# 数据库与接口优化总复核（2026-10-13）

**审查快照：`723394e5b99b8cea7a41f59ba507243282270446`；比较基线：`16867bff8fea89688126cb869adbec5bda6f6460`。** 后续文档提交不改变这个代码快照。

本文是数据库／接口优化的唯一当前状态表，取代 10 月 11 日的 API 初审、复核、A/B/C 实施拆分计划及 10 月 12 日逐轮追加待办。它不是全项目所有历史问题已关闭的声明。后续实施只读 [收敛计划](<../superpowers/plans/2026-10-13-database-api-followup.md>)；维护任务合同见 [DB8](<../superpowers/plans/2026-10-12-db8-maintenance-retention.md>)。

## 1. 结论与证据边界

**主要优化已接入生产，但 DB8 仍为部分交付，不能写“全部完成并验收”。** 两条涉及清理控制的高优先级缺陷、预览／报告错误、配置原子性和有界执行缺口仍存在。没有证据表明本次发现了正在发生的生产事故或跨用户泄露。

方法：三个独立只读审阅分别覆盖数据库、接口与全部 docs；父审阅核对关键调用链。检查了路由→handler→Store/Queue、前端消费、计划与 ADR。数据库 SQL 的两个小型复现使用 Python 的临时内存 SQLite，**不是项目 bundled SQLite 的性能测量**；控制流缺陷由源码确定，尚需公开 HTTP／Queue seam 的回归测试固定。

本轮新验证（不是沿用聊天记录）：

| 验证 | 结果 | 边界 |
|---|---|---|
| `"$HOME/.cargo/bin/cargo" test --workspace` | exit 0；226 套件，1666 passed，0 failed，4 ignored | 证明现有自动回归通过，不证明下列未覆盖时序正确 |
| `pnpm -C web typecheck` | exit 0 | 不是浏览器交互、视觉或真实客户端验收 |
| Rust 文件体积扫描 | 只有 [cli.rs](<../../crates/api/tests/management/cli.rs>) 820 行超 800 | 既有违规；未将“无新增超长文件”说成所有函数均合规 |
| SQL 形状复现 | 确认保护计数截断／重复累加与扫描量随历史增长 | VM 工作量证据，不是生产延迟或 SLA |

没有运行 live 数据清理、真实 ffmpeg、外网、Docker canary、VidHub／Infuse 实机或新 release 性能矩阵。旧 red→green 的口述记录只保留为历史描述；不能为新加测试补写未实际观察的 RED。

## 2. 当前完成度（Spec 轴）

“已实现”只表示所列功能和现有回归存在；“部分”表示原验收仍有明确缺口。原 ID 保留，避免重新编号后丢失问题。

| 原项 | 现状／关键提交 | 未完成或限制 |
|---|---|---|
| A1 默认 Filter 响应 | 已实现 `f63c404` | PATCH 返回当前 `is_default`；未宣称整个 PATCH 改为任意字段增量 |
| A2 directory 原子写 | 已实现 `110eeec` | 只覆盖同 app 连接事务，不是跨文件事务 |
| A3 登录 offlock＋CAS | 已实现 `7b9eb54` | 管理员创建／改密仍内部 hash；不是“所有密码路径离锁” |
| A4 arrivals 快照 | 已实现 `7d999e6` | 冷缓存自锁修复；暂停 Subscribe 表征需核验覆盖 |
| A5 慢入口隔离 | 部分 `76626c2` / `718ee10` / `c92cc22` | 通知、Downloader 动作等已迁移；artwork 候选同步 catalog 漏口见 F10；不能说全部外部工作都有统一预算 |
| A6 字幕 | 主能力已实现 `03a49ed` / `7b832c3` / `3792709` | R1 通知丢失、R2 选举、R3 暖缓存复核已修；R5 任意外部同路径替换仍是设计边界，timeout/freeze/两协议 HTTP 验收不足 |
| B1 归属与批读 | 已实现 `ce5b31f` | 可见性留 API，是记录过的调整；owner 不是授权判断 |
| B2 Library 浏览快照 | 已实现 `75d8a90` | items/index/gallery 的 NFO/stat 已离 Store 锁；仍全候选富化后分页，无持久 NFO 缓存 |
| B3 Playback 批读 | 部分 `a2430ba` | collection favorite 修正、批 ledger/Media 已有；历史／统计的 visibility 与 poster stat 未全面离锁，见 F09 |
| B4 facets／分页 | 已实现 `38e7a2c` / `e81f4ad` / `df48937` | 十维 distinct Media、自身维排除；缺省 tier=all、候选数据驱动；省略 limit 保留全量，不等于默认分页已迁移 |
| B5 历史 SQL 分页 | 核心已接 API `ccee43c` / `361fbc2` | 20 万 cap 已消除；错误→200 空页、输入检查、可见集合成本仍待修 F09/F11 |
| B6 watch SQL 聚合 | 核心已实现 `ae86101` | 多条聚合 SELECT（每窗六条＋hidden），不是单条全能聚合；全锁／大历史性能验收不足 |
| C1 Jellyfin 范围 | 核心已实现 `8785e4f` | Items 未知父范围空页、用户路径校验；Genres/Studios 未统一未知 ParentId，非隐藏数据绕过 |
| C2a 本地元数据／Backdrop | 部分 `e6af6d0` / `2c64a89` / `cda0946` | NFO 离锁收益真实；逐行 ownership/Media 查询仍有，own-backdrop no-enumeration 专项测试缺失 |
| C2b 封面 CAS | 主路径已实现 `6beec32` | legacy 就地写破坏不可变保证 F08；孤儿版本 GC 未做 |
| C3 图片 validator | 已实现 `e6af6d0` | 先授权后 304，静态图保留 public；不是所有图策略统一或所有真实客户端已验收 |
| C4 Jobs watcher／批读 | 部分 `2716e5e` | 共享 poller 与锁拆分完成；批 last-child 仍解码全部历史 F07 |
| C5a HTTP 观测／流式日志 | 已实现 `c1a4306` / `cd8fe57` | headers latency 非 body 传输耗时；未匹配 404 观测和 Jellyfin 完整观测不是本次已交付 |
| C5b release 验收 | 部分 `1dc081e` | 有历史 before/after，但 gallery 未达原 ≥30% 目标，未测 20 万历史／DB8 并发影响，见 §5 |
| DB1／DB2 | 已实现 `2b51862` | 日志 token 脱敏、构建上下文排 secret；不是所有错误链泄密已穷尽证明 |
| DB3／DB4 | 已实现 `fef3d6e` | 四 Store 库新版本拒绝、wanted 批事务；不是 Queue/cache schema 统一迁移或文件系统绝对零写入保证 |
| DB5／DB6 | 索引已实现 `86310bc` / `11708be` | 热查询 EQP 证据真实；完整 endpoint 成本仍须含批读、索引写入与体积成本 |
| DB7 | 已实现 `6071b0c` | WAL/5000ms busy/FK 显式；跨文件无 FK/事务，未声明的 FK 不凭 PRAGMA 自动产生 |
| DB8 | **部分交付** `c03f39c`…`d1c8205` | 三任务、策略、删除、执行器、API/UI 均在；F01–F06/F12 是剩余缺陷／验收门 |
| DB9／DB10 | 后端主能力已实现 `71ce9e9` | env pin 409、解析统一／_FILE；UI 未禁用 env_override，空 DATA 仍回落，列后续 |
| DB11 | 未实施 | typed settings 注册表、Indexer overlay 版本／未知字段策略，结构性任务不直接大改 |
| DB12 | 未完成测量门 | 冗余索引逐查询／体积／写成本复测后才可 DROP |
| DB13／DB14／DB17／DB19 | 已实现 `f567bcd` / `b7e82e1` / `675ed53` / `16df3e7` / `aa1c8a6` | AuthUser、配置 Debug、代理口令脱敏；旧文档“AuthUser 仍 derive(Debug)”错误 |
| DB15／DB16／DB18 | 测试同步修正 `917c17c` / `58233af` / `4670239` | 特定历史 flake 证据，不是所有生产竞态不存在的证明 |

## 3. 仍有问题的实现（按后果分级）

### F01 · P1 · 关闭正在运行的清理仍继续删后续批次

[取消检查](<../../crates/api/src/maintenance/runner.rs#L121-L130>)只看 Job status/attempt；[禁用定义](<../../crates/jobs/src/store.rs#L330-L342>)仅取消 queued，不改 running。管理员在第一批之后关闭，后续批仍会删除，直到本轮最多 10000 行。与 UI／合同“关闭阻止后续批次”不符。

修复：在批次／库边界检查本 def 是否启用；当前事务不强杀、不回滚已提交批。公开 seam 用 gate 等第一批提交→禁用→释放，断言不启动第二批；另测独立无 def Job。

### F02 · P1 · 更安全的新策略不应用到已排队的下一次执行

[spawn_follow_up](<../../crates/jobs/src/schedule.rs#L105-L129>)在前次完成时读取**当时def.payload**并立即创建queued子任务（不是无条件复制running Job的旧payload）；[保存配置](<../../crates/api/src/http/job_maintenance.rs#L115-L157>)只更新 def。把 90 天改为 365 天或启用 dry_run 后，已经排队的下一次仍按旧策略实删。这不是“运行中保持快照”的合理行为，是尚未运行的任务未刷新。

修复：同一配置事务重写／安全重新排期 queued 维护子任务；running 快照不变。并发 claim 与保存只允许完整旧配置或完整新配置，不允许混合。与 F04 一起交付。

### F03 · P2 · 保护数量截断不标记，跨批重复相加

[protected_window](<../../crates/jobs/src/retention.rs#L124-L148>)最多返回 501，没有保护计数截断标志；[preview](<../../crates/api/src/http/job_maintenance.rs#L210-L224>)仅以可删行 has_more 决定 truncated；[runner](<../../crates/api/src/maintenance/runner.rs#L315-L319>)每批累加同一份保护窗口。

复现：600 保护／0 可删→`protected=501,truncated=false`；600 保护＋1200 可删→500/500/200 三批各报 501，汇总 **1503**，实际保护 **600**。因此不能把当前 protected 当精确总数或不重复计数。

修复：分开候选／保护的计数范围与截断标记；保护只计一次或按稳定游标去重。返回下界必须显式标志，UI 不叫“完整窗口”。

### F04 · P2 · 配置 CAS 不是整个配置原子更新

[handler](<../../crates/api/src/http/job_maintenance.rs#L115-L173>)先提交 payload CAS，后另锁写 schedule；schedule 错误时 HTTP 500 但 payload 已生效；并发请求可拼出 B 的 payload＋A 的周期。仅改 schedule 时 payload token 不变，也不能检测陈旧周期更新。省略 interval 时响应 schedule=null，实际周期未清空。

[旧schedule写入口](<../../crates/api/src/http/jobs.rs#L454-L515>)也未CAS：更新周期后取消queued，再全局ensure_scheduled(now)，可能立即补任务；只修新端点会被旧writer绕过。

修复：事务覆盖payload＋schedule＋queued刷新；使用覆盖完整配置的独立revision/token，迁移维护任务的旧schedule客户端或拒绝该无token写入口。返回数据库实际状态，失败整笔回滚。

### F05 · P2 · LIMIT 限制输出，不限制扫描／锁时间／async 工作

[Jobs 候选与保护 SQL](<../../crates/jobs/src/retention.rs#L106-L148>)可为寻找 501 个匹配项扫描全部超龄历史；[Playback](<../../crates/store/src/playback/retention.rs#L112-L125>)按 ended_at 过滤却沿 started_at 扫描。没有 SQL 扫描／时间预算。[preview](<../../crates/api/src/http/job_maintenance.rs#L178-L273>)在 async handler 直接持全局 Queue/Store 执行同步 SQL。

内存 SQLite 复现：全保护 1k/10k/100k 行的可删查询返回 0，约 39k/390k/3.9m VM steps；全独立历史的保护查询返回 0，约 12k/120k/1.2m；无过期 Playback 约 4k/40k/400k。只证明增长规律，**不换算毫秒或生产 SLA**。

修复：独立 blocking 准入＋真实扫描/时间预算、稳定游标和恢复；Playback age 索引须用 bundled SQLite EQP／写成本验证。不能只再包 Tokio timeout 后丢工作，也不能用 cursor 越过行后永远漏删。

### F06 · P2／发布决策 · VACUUM 没有硬 30s 停止保证

[执行器注释与实现](<../../crates/api/src/maintenance/database.rs#L34-L46>)已披露 backup-copy 阶段不响应 interrupt；[执行与 join](<../../crates/api/src/maintenance/database.rs#L233-L264>)等待真实结束。[active attempts](<../../crates/api/src/job_loop.rs#L113-L121>)不被超时 recovery 回收，因此 900s 也不是终止器。

默认 VACUUM 关闭是降低风险，不是硬期限验收通过。需单独批准：保持离线／禁用重型路径，或接受明确的 best-effort 契约，或用经崩溃恢复验证的隔离执行器。不能让文档把“已披露偏差”视为用户已批准放弃安全目标。

### F07 · P2 · Jobs 批 last-child 读仍解码所有历史

[collect_last](<../../crates/jobs/src/read.rs#L82-L108>)对所有所选 def 读取全部终态行，再在 Rust 留第一条。[列表 limit](<../../crates/api/src/http/jobs.rs#L239-L304>)在获取 children 后才起效。一个 def 的 20 万条历史仍会全部解码 payload/progress 并占 Queue 锁；SQL 次数下降不等于总工作量下降。

修复：实际索引 top-1-per-def 查询／等价分组查询，保留现有 tie 语义；无关旧坏行不应让最新 child 列表整体失败。单 def 大历史／多 def fixture 复测。

### F08 · P2 · legacy 封面先覆盖当前文件再 CAS

[ensure_cover](<../../crates/api/src/library_cover_service.rs#L86-L105>)在 legacy 升级分支先写 currently referenced 文件，再 publish/CAS。渲染期间 freeze／CAS 失败／DB trigger 拒绝后，旧文件 bytes 已变，不能兑现“失败旧 bytes 保留”。兼容 carve-out 已披露，但它仍是保证缺口，不能归为无害风格。

修复：legacy 同样新版本文件→lease publication＋CAS→保留旧引用；失败只清未引用版本。补 legacy 分支的 gate/trigger 测试，不能只测正常新版本路径。

### F09 · P2 · 历史／统计与 Jellyfin DB 快照仍有 N+1／锁内 stat

[visible_media_ids](<../../crates/api/src/http/playback_logs.rs#L75-L94>)全 ledger→逐 row Media／owner／权限查询；[history](<../../crates/api/src/http/playback_logs.rs#L114-L179>)与统计仍持 Store。名为纯渲染的 [media_target](<../../crates/api/src/playback_read.rs#L89-L106>)调用 poster_path→is_file，仍有锁内文件探测。Jellyfin owned NFO 离锁已做，但 [collect_pending](<../../crates/api/src/media_server_provider/items.rs#L126-L180>)仍逐 row 查询。

修复：共享请求级 ownership／Media／角色快照，owned poster facts 锁外 blocking；不能放松多库同 Media、嵌套／歧义根、freeze 可见性。SQL 聚合避免 20 万加载的收益保留，不为减少查询条数牺牲正确性。

### F10 · P2 · artwork 候选仍在 async worker 同步 catalog

[artwork_candidates](<../../crates/api/src/http/library_artwork.rs#L41-L112>)在 async 中直接 poster_url，且前置 poster stat 在 Store guard 内。慢 catalog 仍阻塞 worker；这是原 A5 未迁移完的入口，不是本轮新增回归。

修复：完整 owned 用例进入现有 BlockingBudget，释放 Store 后运行 catalog/stat。gate fake 下验证无关 HTTP／Store 可推进。

### F11 · P2/P3 · 历史查询失败伪装成功空页，极值输入未受检

[history](<../../crates/api/src/http/playback_logs.rs#L103-L151>)SQL 错误转 200 空页；before 的 i128→i64 强转可回绕，days 乘减未检查。这些行为不能因历史已有而说 B5 的“queryerror500/checked cursor”已完成。

修复：明确 400 参数语义、checked arithmetic；查询故障返回标准 5xx，不伪造无历史。不要把正常空可见集当故障。

### F12 · P2/P3 · UI 预览陈旧、运行报告不可见，tick 修改维护状态

[维护抽屉](<../../web/components/maintenance-job-settings.tsx#L58-L65>)修改策略不清旧 preview，晚返回请求也不绑定 draft；旧计数可显示在新保留期旁。last_report 仅类型／parser，组件未渲染。[DevHttp tick](<../../crates/api/src/job_loop.rs#L125-L161>)会取消后台 queued 维护 Job，最近终态 progress=NULL 又遮住此前报告；下次 background ensure_scheduled 可立即补新 job，**不能推断必然推迟完整一个 interval**。

[run_job](<../../crates/api/src/http/jobs.rs#L394-L405>)遇到未来 queued 子任务直接返回 queued:false，不再使用旧 Queue 手动提前到 run_after=0 的能力。周期任务通常已有 future child，因此“立即执行”可能实际上不提前。

修复：preview 绑定策略签名／generation；渲染当前报告及截断／超限原因；DevHttp 跳过不应取消后台维护任务；手动触发 queued 的既有任务应安全提前，running 仍不新增。

## 4. Standards 轴（独立于 Spec 轴）

1. **测试充分性不足**：控制边界必须有公开行为回归，现有绿测没有覆盖 F01/F02/F04/F08/F12 的关键时序。以拆文件、trait seam 或 typecheck 替代这些验收不符合 [AGENTS](<../../AGENTS.md>)。本轮不伪称已写 RED→GREEN。
2. **错误可观测性不完整**：[冷却读取](<../../crates/api/src/maintenance/runner.rs#L243-L250>)吞 read/parse 错误当“无上次 VACUUM”，可能放宽冷却；[PRAGMA 诊断](<../../crates/api/src/maintenance/database.rs#L231-L231>)及后续读取部分静默默认。应保守 skip＋结构化错误，不打印秘密。不是把所有 optional None 一律当 error。
3. **硬尺寸既有债务**：[cli.rs](<../../crates/api/tests/management/cli.rs>) 820 行。domain 主文件超软限是组织建议，不是硬限违规；未把 lint-only 未用参数作为功能 bug。
4. **重复映射等气味**：genre/country label 与 parser 多处维护，属于可能的 Duplicated Code；建议统一词表，但不据此推断用户已得到错误结果。

## 5. 性能与验证未完成项

历史 [C5b release 数据](<../benchmarks/2026-10-12-api-endpoints.md>)有价值，但必须连限制一起引用：

- 相同 mode 的 before/after 可比较；movie/tv 之间 fixture 不对称，TV 没命中 tvshow.nfo，不能把差距归为产品 facets bug。
- facets DTO 已由旧占位变十维，不能以字节／耗时直接说同负载退化；单趟计数在该 fixture 上接近中性。
- 原目标是 2000 行 items/search/gallery median 至少下降 30%。movie gallery 381.27→297.36ms 约 **22.0%**，TV 305.33→230.43ms 约 **24.5%**，**没有达到目标**；1.3×加速不等于 30% 延迟下降。
- 原 history/stats 只有 100–10000 条日志。20 万边界的正确性 fixture／DB8 20 万播放日志删除两次各 10000 的 **45ms debug 历史测量**，不是 20 万 history/stats release 性能，也不是维护下 HTTP p95。
- [DB5](<../benchmarks/2026-10-12-jobs-query-plans.md>)只证明所测热查询；[DB6](<../benchmarks/2026-10-12-media-identity-indexes.md>)未测全部写成本，不承诺“可忽略”。

未做：修正 NFO 对称的多 Library movie/tv/mixed release 矩阵、20 万＋历史/统计、维护开启/关闭并发 HTTP median/p95、Store/Queue 锁等待、WAL/主体大小、真实客户端抓包与 Range/取消/吞吐实验。连接池、persistent read model、DROP 重叠索引、stream buffer 仍是测量／设计门，不是默认批准开发。

## 6. 不应再继承的错误结论

- root_id 当前不能当已持久化的归属直接 JOIN；同一 Media 可属多个 Library，不能缓存成唯一 Media→Library。
- 四 Store 连接／文件及独立 Queue/cache 普通事务不是跨文件事务；WAL 不等于无全局 mutex 竞争。
- FK／busy_timeout 当前显式化之前，bundled 默认实测已启用 FK／5000ms；缺显式语句不等于“FK 完全无效、timeout=0”。
- TTL 是 freshness，不是安全删除期限；ADR-0008 stale-if-error 必须保留，catalog/fanart 缺安全孤儿映射。
- 非唯一 alias 索引正确；Movie/TV 同数字合法，不能全局 UNIQUE；重叠前缀不自动等于冗余。
- SQL LIMIT／批数限制不证明扫描、持锁时间有界；独立维护连接不保证前台零延迟。
- public 静态图片是约定，不因认证看似严格就迁至 member；Jellyfin 字幕／视频按协议鉴权。
- POST 搜索可以实际排 Job，诊断 200＋data.ok=false、DELETE 未命中幂等成功不必然是 bug。
- 没有实际客户端调用证据不能补齐整个 Jellyfin、改 Limit=0/Recursive；没有吞吐测量不能宣布 4KiB 是故障。
- R1/R2/R3、AuthUser Debug、无 scheme proxy 口令等已修，不再放到“当前待修”；历史样本行数不是当前 live 数量。

## 7. 全 docs 收敛范围与历史债务

[文档目录](<../README.md>)标明每份文档的用途。删除的是已迁移关键信息的 API 初审误报清单、被本表替代的逐轮状态／A/B/C 旧执行计划及易覆盖历史证据的临时 repro 复制品；不是按日期删除全部 ADR 或独立计划。

09-29／10-05／10-10 全项目审计仍是**历史、未逐 ID 对账**，不能因 API 优化绿测就写全部关闭，也不应把每条历史候选自动重开为现存 bug。独立 rename、adaptive fingerprint、probe retry、Library removal 等已有实现提交但仍需分别核对实机／恢复／取消验收；本轮只标注状态，未替这些领域做全量重验。

后续优先序：F01→F02/F04→F03/F05/F06→F07–F12→测量／配置收尾→历史审计逐ID对账。详见 [唯一后续计划](<../superpowers/plans/2026-10-13-database-api-followup.md>)。本文没有自动批准生产修复或真实数据删除。

文档独立复核结果：Standards未发现必须修改项；Spec提出两条计划缺口——完整config CAS必须覆盖旧schedule writer/client，shutdown/drain必须有实现归属而非仅验收步骤。两条均已补到T2/T1；同时补T1–T5→T9依赖、预览“行数预算≠工作预算”、follow-up复制def当前payload的精确描述。没有为此声称生产缺陷已修。
