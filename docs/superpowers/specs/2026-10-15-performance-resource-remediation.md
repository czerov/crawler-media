# 数据库、接口性能与内存资源修复设计

日期：2026-10-15。状态：**待用户 review；未实施、未授权生产清理。**

**来源：** [完整审查 DB-01–07 / API-01–05 / MEM-01–05](<../../audits/2026-10-15-database-api-memory-review.md>)。证据是当前源码和合成实验，不能推出已达性能目标。

## 1. 目标与非目标

目标：消除明确的资源许可/缓存清理错误；减少无关数据导致的 SQL/文件工作；把读取、执行、排队和历史容量分别限制；保持身份、归属、可见性、进度、失败重试和删除安全。

非目标：换 PostgreSQL/Redis、跨文件强行事务、重写队列框架、默认启用删除/VACUUM、修改省略 limit 的全量兼容行为、静态图片加鉴权、放开 Playback 流认证、修正并提交用户正在进行的 Probe 修改。

三个独立子计划：数据库、接口、资源；总计划协调共享 Store/ApiState 文件。阶段验收独立，不把完整项目捆成一个巨型提交。

## 2. Global Constraints（子计划原样引用）

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

## 3. 行为与容量决策

### 3.1 两处 SQL 和 Probe 计数

- 详情前缀改 `cache_key = prefix OR (cache_key >= prefix+'?' AND cache_key < prefix+'@')`，合法详情前缀由源/类型/ID 构造，不接受任意 SQL。保留 fetched_at/rowid tie。验证 `%`、`_`、相近数字、语言变体、子路径隔离。
- 探测义务新增 `(created_at_ms,ledger_id)` 索引，原方法 limit 仍 `1..=100`；迁移幂等且先做新版本拒绝保护。
- ProbeJob 新持久 `partial` 计数、API是否暴露沿现有DTO不变；仅有效 unit 转移按 old/new delta 更新 completed/succeeded/failed/partial。cancel/recovery 统一对账，错误提交不扩大计数。

### 3.2 owned 读取与严格边界

- 一次 LibraryOwnership + 角色 + Media batch；不把 Media 绑定为单一 Library，同 Media 的多个物理拷贝仍可属于不同库。
- Subscribe：锁内 saved facts/ledger/quality/config revision；锁外 LibraryPathScope + 文件核验；副作用前检验相同结构/source version，不沿用过期授权。根解析遇权限/IO错误必须fail-closed，不能跳过不可解析的更深根而把文件授予更宽根；不存在后缀的既有词法解析语义保留。
- 路径核对：每 root 一次目录遍历（深度≤5保持），basename→候选集合；不跟随 symlink 逃出范围，预览最多检查 100,000 项，等待/协作检查预算30秒，截断明确 `incomplete=true`，不假装查全。单次FS调用不可硬中断，timeout不提前释放worker许可；应用重关联仍独立安全验证。
- 普通页面默认沿现有 30 秒等待预算/8 个 http_work 容量；容量不足503、等候超时504，不静默变慢路径。

### 3.3 Jellyfin 和 REST 分页

- 第一阶段批量 owned snapshot + blocking，把 N×配置重读和 async 同步 IO 消除；此时若仍读所有候选，明确标“部分修复”。
- 第二阶段只构建本页重型 item metadata；NextUp 一次聚合所有候选 unit，不循环全库 provider。Genre/studio/person/filter 在选页之前，准确 count、stable tie、folder/unknown parent、显式 IsPlayed 与默认 played-last 不变。
- REST 普通 title/added_at 无 watch/filter 查询首先 SQL 选 Media IDs，再取本页 files/units；release_date 仅按现有完整日期/年份兜底语义物化后优化，不新增 `sort=year`。需要高级过滤时可读取轻量每 Media 候选事实（不取全库 file facts/NFO），过滤排序后才取本页重事实；对此残余 O(M+U) 诚实披露，不能宣称所有过滤 seek。
- `limit` 显式 `1..=200`、省略全量响应行为不变；不悄悄默认200。OFFSET 深翻页有剩余成本，cursor API 另决策。
- File/meta facts 真正解码只限本页；投影 freshness 查询只读 generation/dirty token，不顺手装载整个 projection。
- Library stats 使用一次批量归属聚合及投影 file facts；增加 `stats.refreshed_at_ms` 与 `stats.stale`（兼容新增字段）。未就绪以首次有界构建获取完整结果，不把缺统计假写0。
- up-next movie/Video 不调用 season；卡片不等外部 Metadata。TV 名称来自 NFO/现成 cache；缺失名称返回现有文件名/空值语义，后台用 Catalog cache 及 search_work 的4容量单飞补齐。

### 3.4 投影依赖与周期刷新

- 原全局 revision 保留用于降级/升级保护，但热页 freshness 改派生的每库 dirty/generation token。
- 每个源库用 durable `_browse_changes(seq,entity_kind,entity_key,old_path,new_path)` 记录 INSERT/UPDATE/DELETE；app Media、Library 配置、library ledger/file_meta/marker、catalog detail 均覆盖。catalog search/discover 明确不影响 Library 详情，未知键保守标脏。
- library 中 `browse_change_cursors(source,last_seq)` 与受影响库 dirty 状态同事务落盘；**先持久化 dirty，再推进 cursor**；源事件清理仅清已确认 seq，崩溃可重复处理，不丢失。
- 每轮最多512事件；未消费完时对应源“不完整”，不能把未验证代当 fresh；容量/预算失败留 dirty，retry不造第二构建。
- 受影响库由旧投影依赖+当前归属共同解析；删除/移动同时失效 old/new owner；配置变更全受影响scope，不依赖未填充root_id。
- 60秒外部文件检查保留：离锁检查已访问库 NFO/图片/视频版本，只更新变化，不无条件重建/全代写；远程 FS stat 成本仍随文件数增长，freshness上界仍是检查周期+工作延迟，不能承诺硬60秒。
- 发布在同一 library事务检查dirty token/scope再patch；构建期变化不吞新dirty。首次/损坏/升级允许full rebuild，正常稳定不重复整代写。

### 3.5 海报容量与生命周期

- 实际上游工作并发上限 **4**；外层等待仍 **10秒**；同Media单飞直到worker真结束。worker持 OwnedSemaphorePermit，取消后不重复fetch。
- negative TTL仍 **30秒**；最大条目 **4,096**；miss/insert全局扫过期并LRU淘汰，测试使用注入时钟。服务存活不等于允许无限失败ID保留。
- gate注册表用 Weak/RAII身份管理，活跃worker/等待者仍强持共享gate；每次访问清理dead Weak，注册表上限 **4,096**，全活跃满时明确503，不创建平行gate。
- 磁盘cache工作也纳入有界预算；图片现有8MiB上限保留。timeout结果未知，不能声称工作未执行。

### 3.6 章节与 Probe 的有界窗口

- 章节：实际 ffmpeg worker **2**，内存 pending ID 窗口 **64**；持久义务为真源，超容量只延后，不丢chapter生成请求。按 `(ledger_id,source_version,targets_hash)` 唯一；图片已存在且版本正确跳过。
- 持久chapter义务只描述生成工作，不复制 marker 权威事实；完成删除义务，cancel/freeze同现有LibraryWorkRegistry收尾；crash running恢复queued。无runtime只留持久义务，不另开无界thread。
- Probe：metadata内存 pending **64**、fingerprint pending **16**；metadata active沿现有配置 `1..=8`、fingerprint active **1**。channel放有限任务ID，不整队列复制 LedgerRow/PCM。
- 持久probe stage是完整状态来源，派发满不ack丢工作；metadata完成→fingerprint pending持久转移后再wake；重启只装窗口。执行时重新读unit/source，保持 job_id防晚到消息。
- 元数据优先行为保留，不在本轮改变声纹公平策略；长期metadata产生导致声纹饥饿在验收中记录，不以扩大内存掩盖。
- worker启动幂等；提供可观察shutdown/drain，防同runtime重建测试永远保留旧manager；不据此宣称原单实例生产泄漏。

### 3.7 分类历史保留（不自动启用）

新增一个 `data_retention` JobKind，复用既有维护配置CAS/批边界控制，不建平行scheduler。def `enabled=false`、payload `version=1,dry_run=true`、周期86,400秒。各类固定默认90天、范围1..=36,500；500条/批、最多20批或2秒（先到先停，单SQL不能被这句话假称可硬终止）。

- QoE：`recorded_at`稳定keyset，不影响playback_units/session/progress。
- catalog：仅已分类 transient search/discover 过期且 fetched_at超过90天可删；transient目标容量10,000是收敛目标，预算耗尽可暂超限、显式remaining/incomplete。未知和详情全部保护，详情stale不按TTL删。
- Probe：仅 terminal且>90天，按scope至少保留最新3（时间tie全部保护），活动job/units/被sample.captured_job_id引用的job全部保护。
- attempts：仅其job可安全清理后级联按job_id删；不单独按不存在的source_version列分组。fingerprint_samples/models/model_members本轮不删。
- Preview/dry-run不改行；关闭在下一批生效，错误/未知版本拒绝不回退激进默认；SQL有索引游标，单批工作量随窗口而非全历史增长。

## 4. 设计门 DB-07

默认Library未匹配回退不可seek先单独记录benchmark。候选方向是 `browse_ledger_owners` **可重建派生索引**（library_id、ledger_id、kind、ownership_revision），不是把root_id补填为新的删除权威事实。修改结构需独立ADR增补、词汇/跨库恢复合同review；未批准只交设计/benchmark，DB-07保持open。

## 5. 验收与兼容矩阵

- 合成规模：SQL 1k/10k/50k；接口100/2k/10k Media、1/5/20 Library、最多50k ledger；Probe恢复100k pending；章节200个文件；负缓存10k不同失败ID；同signal 1k完成请求。
- 冷/热、有无NFO、movie/tv/mixed、multi-copy/nested/ambiguous/hidden/frozen、缺失文件、同路径替换、平局时间/标题、过期Metadata错误、客户端取消都必须有测试。
- 工作量门：catalog/obligation固定目标从1k→50k VM倍率≤2；Probe整轮100→1k unit VM倍率≤15（而非平方）；热页50k→100k无关文件，重facts解码只限本页；NextUp全候选读不随S重复；worker/进程和pending严格≤配置。
- 所有接口JSON字段/名单/次序/TotalRecordCount与旧语义等价，允许上述stats兼容新增字段；复杂过滤残余量如实记录。
- Release前后同fixture/seed、每格至少5轮，报告median/p95、CPU、RSS峰值/静止后、线程/子进程、Store锁等待/持有、SQL VM/decoded rows、DB/WAL大小；未测填Not run，不写预期收益为结果。
- 单请求超时不是工作取消，证明取消后真实任务/permit/lease/reader最终回收；重启与freeze保留恢复语义。

## 6. 五个重点失败模式

1. 隐藏/嵌套/歧义根、同Media多库拷贝：分页/count和统计不越权，strict副作用不回退（A1/A3/A4/A5）。
2. 请求取消/超时后工作未结束：真实并发不越上限、同key不重复（R1/R2/R3）。
3. 快照离锁时删除/改root/同路径替换：旧观察不授权新动作、不复活数据（A1/D3/R3/R4）。
4. 平局分页/高级过滤/缓存损坏：不重复漏项、不分页后再过滤，错误不伪装空集合（D1/A3/A4）。
5. 满队列/恢复/禁用清理：不丢持久任务、不删active/引用数据，停止和remaining真实（D4/R3/R4）。

## 7. 文档自审与交接

覆盖矩阵在 [审查总表](<../../audits/2026-10-15-database-api-memory-review.md#L17-L40>)；所有17项都有独立任务/设计门。接口签名是**计划新增接口**，不是声称源码已有。执行者先读审查+本设计+所属子计划；实际工具没有旧skill名时用环境可用的executing-plans/公开seam TDD，不安装不存在技能。
