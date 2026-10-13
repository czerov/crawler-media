# 第三批：接口与数据库有界性能、扫描执行治理技术设计

- 状态：待审阅，未实施。
- 前置：[第一批](<2026-10-13-library-data-safety-batch-1-design.md>)、[第二批](<2026-10-13-library-correctness-batch-2-design.md>)验收通过。
- 配套：[第三批开发计划](<2026-10-13-library-performance-batch-3-plan.md>)；[总路线图](<2026-10-13-database-api-scan-master-plan.md>)。

## 1. 性能目标与不变量

优化不是换数据库或盲目加索引。优先消除不相关全库读取、全局锁内FS IO、逐文件重复归属、无界并发及retention重复扫描。保持前两批用户事实、安全scope、会话ID、probe obligation合同。

硬性验收：

1. 热浏览items/gallery请求不全量读取catalog body，不做NFO/read_dir/逐文件stat；分页以页面大小控制实际图片渲染。没有limit的旧合同仍返回全部，不伪称有界响应。
2. 单Library请求不读取其他Library所有ledger或当前用户全部units；可见性用批量事实一次构造，不逐row重载配置。
3. 任意文件IO及blocking SQLite作业不在Tokio worker直接执行；Store锁内只有短数据库读取/写入与纯内存判断。
4. 每次扫描登记chunk最多100个文件；每轮预算5000个目录项或10秒，任一达到yield；每次retention外层最多检视500行，保护行也算visited，跨轮游标不被protected前缀饿死。
5. 只读HTTP共享预算8；搜索独立预算4、文件目录浏览独立预算2；scan全局最多2、同Library仅1活动扫描（root配置/冻结仍复核）。不改变有副作用操作必须run_to_completion。
6. 数据库新增索引必须有对应原SQL/EQP/边界测试，不要求优化器输出固定字符串。

Release性能目标（比较同机器/同fixture的实施前后）：10k Media items/gallery/history中位数各至少改善30%；jobs列表不得恶化超过20%。这是待测发布门槛，不是已证结果；若算法指标达到而相对延迟未达，必须定位并报告，不隐瞒为完成。每格至少30样本；真实NAS/设备另列Not run。

## 2. 浏览派生快照：先可靠，再分页

### 2.1 不拿未backfill root_id当事实

新增library.db可重建投影；schema **8→9**（第二批已用8）。不修改ledger.path语义，不将投影视为Media/Library第二份权威数据。

```sql
CREATE TABLE browse_generations(
 library_id TEXT PRIMARY KEY, generation INTEGER NOT NULL,
 scope_signature TEXT NOT NULL, state TEXT NOT NULL, refreshed_at_ms INTEGER NOT NULL,
 app_revision INTEGER NOT NULL, library_revision INTEGER NOT NULL,
 catalog_revision INTEGER NOT NULL, subscribe_revision INTEGER NOT NULL
);
CREATE TABLE browse_files(
 library_id TEXT NOT NULL, ledger_id TEXT NOT NULL, media_id TEXT NOT NULL,
 generation INTEGER NOT NULL, facts_json TEXT NOT NULL,
 PRIMARY KEY(library_id, ledger_id)
);
CREATE INDEX browse_files_media_idx ON browse_files(library_id, media_id);
CREATE TABLE browse_media(
 library_id TEXT NOT NULL, media_id TEXT NOT NULL, generation INTEGER NOT NULL,
 title TEXT NOT NULL, year INTEGER, facts_json TEXT NOT NULL,
 PRIMARY KEY(library_id, media_id)
);
CREATE INDEX browse_media_title_idx ON browse_media(library_id,title,media_id);
```

facts保存已校验的显示字段：原有Media字段、NFO/catalog补充的genres/countries/rating/runtime/language/release_date/scraped，文件质量/season/episode/missing，文件size、created/modified时间，poster/fanart/still存在性及URL。时间沿用旧created优先modified兜底、作品added_at取最早；.strm体积统计保持0；图片同现有preferred_row选取规则。不得存token/凭证。

generation发布：锁内抓target Library的strict scope/ledger/media事实→锁外NFO/stat及定向catalog keys→写临时generation→lease.publish下核对scope与依赖revision→一个library.db事务替换当前generation并标ready。失败保留旧generation，不把半更新与新revision标ready。

第一版索引按Library定向；不全量cache刷新。catalog仅查询目标Media详情键前缀，参数匹配需按kind/id边界，不用contains(id)误配。相同详情多language沿用现有fetched_at最新优先，明确稳定并列键。

### 2.2 失效源不能遗漏

引入app/library/catalog/subscribe各自事务内revision触发器，只针对浏览相关表DML递增；投影自身写入不递增。library ledger/file_meta/markers、app Media/Library roots/settings中浏览配置、catalog payload改变、subscribe units改变均覆盖；各库revision是独立序号，不伪称跨库原子快照。

- app根/blocked变化：旧generation不得用于授权；实时权限/冻结始终从当前配置决定。
- ledger增删/rename/missing与probe结果、NFO/Scrape/海报写成功、STRM URL变化：标target dirty。
- catalog clear/更新、Media rebind/alias变更：通过revision触发相关Library重建；没有可靠映射时保守失效全部generation，但不在每个请求全量重建。
- 用户units不使公共图片/NFO投影重建，只失效用户派生观看汇总；按当前target media IDs分500批读取units。
- 外部修改NFO/图片：事件标dirty；watch未开启时最多**60秒**周期刷新已访问Library的文件facts，后台有界任务，读请求不逐文件stat。因此外部修改可有≤60秒+构建排队延迟；此一致性改变必须发布说明。

读到未建/dirty generation：由single-flight重建，读请求最多等30秒，容量不足503、等超时504；不可悄悄用过期授权。完整旧generation允许在仅文件facts dirty时stale-while-revalidate，响应增加`X-Library-Projection: stale`；scope/ledger身份变化不能返回旧ledger，必须过滤当前有效ID或等待新代。

### 2.3 分页与筛选语义

普通title/year等可SQL推入的排序/筛选用projection页查询；watch tier、random、复合筛选先读取**当前Library的紧凑Media facts**、批量当前target units，纯CPU过滤/排序后分页。不声称所有查询都是O(page)：facets、watch tier和复杂筛选可O(target Media)，但不再O全实例ledger+所有cache bytes+文件IO。

gallery必须先按投影中的has_artwork剔除空组再分页，不能改成“先分页再剔空”导致短页；只渲染本页图片集合。单Media的超大episode图集仍可能大响应，保持合同并在指标记录bytes，新增图集分页属于后续API版本。

现有stable MediaId tie-break、随机FNV、whole/legacy movie观看语义、收藏继承、过滤值校验均不变。SQL/内存路径对同fixture输出必须一致。搜索无limit旧合同不删除；Web改显式limit分页需单独兼容测试，后端不无声截断。

## 3. 可见范围与锁内IO清除

Playback history/stats：用当前LibraryOwnership、Media batch、ledger归属投影构建visible Media集合；数据库scope all仍仅admin。不是把全user单位移到另一个全局循环。可见集合大时复用temp table/参数chunk，连接内作用域清理，失败不能泄漏上个User范围。

Library列表stats从browse_files聚合，不逐文件metadata。Jellyfin库封面tag在版本发布时缓存；legacy外部cover变更由60秒刷新检测，读请求不得整图hash。item_detail NFO/STRM读取/图片conditional GET实际读取均放预算池；不在Store锁下read/stat/hash。STRM上限**64KiB decoded UTF-8**，超限502固定错误，慢挂载仍无法强制中断底层OS读取，不夸大timeout能取消blocking任务。

所有auth同步SQL转owned blocking短作业；不将parking_lot Mutex简单换成tokio Mutex后继续阻塞IO。先缩小临界区，暂不引入跨库读连接池；若实测锁等待仍主导，再立独立ADR，不把未设计连接池塞进本批。

## 4. durable扫描、单飞与有界遍历

### 4.1 管理接口变化

`POST /libraries/{id}/scan`在本批改为**202**，返回`operation_id,status=queued,queued=true`；旧transferred不再谎报已完成，返回false，新增完成结果由`GET /libraries/{id}/scan-operations/{operation_id}`获得。Web调用方改为轮询/事件刷新，测试更新；不同时保留两个含糊执行模式。

运行状态queued/running/yielded/completed/partial/cancelled/failed；同Library重复请求返回现有operation_id与202，不并发起第二扫描。cancel `POST .../{operation_id}/cancel`管理员；不存在404、非归属404、冻结409。结果沿用第二批ScanReport字段，operation不是probe job完成保证。

library.db新增scan_operations及scan_seen（同schema9迁移），operation保存Library ID、配置signature、状态、generation、计数、时间、错误摘要；scan_seen主键(operation_id,path)，只为遍历去重与恢复，完成后保留报告、清seen。活跃unique library_id部分索引；dispatcher最多2个blocking scan。

### 4.2 恢复规则与边界

使用迭代目录栈，不递归栈溢出，不把全文件集合collect/clone。directory iterator在同一工作轮内持有；每100文件发布chunk，5000目录项/10秒yield。遍历在每目录项及每chunk前检查WorkControl；单次OS read_dir/metadata/NFO仍可能不可中断，30秒drain失败保持冻结，不能报告已停。

重启后不使用不可靠的read_dir offset跨进程续扫；从root重新枚举，scan_seen去重已处理条目，未完成chunk可幂等重做，probe由第二批obligation保障。**不声称目录变动下exactly-once文件快照**；一次扫描中的新文件可能需要下一轮，权限错误报告partial。seen不能让当前已恢复/变化文件永久跳过，restart时仍检查missing/文件版本，changed内容重新处理。

scope配置/冻结变化时旧operation取消，重新手动启动新generation，不沿旧scope接续。所有full/watch/创建自动扫描走同协调器；watch请求target子目录，root全扫描已活动则合并dirty目标到下一轮，不能直接另起裸spawn_blocking。B1严格scope必须在每chunk发布时复核。

metadata阶段按Media batch索引处理当前scope rows，禁止每Media重新rows_in_library全表扫。Video共用第二批格式/reported walker，不再独立递归函数。

## 5. Jobs retention真正预算与SSE常数计数

### 5.1 Retention窗口与游标

不再先过滤deletable再LIMIT。查询超龄终态原始窗口，按`finished_at,rowid`稳定游标最多500行；每行用existing latest/tie规则判保护，保护也算scanned。rowid仅是当前jobs.db游标，不外泄API业务ID。

每job_def在jobs.db保存`retention_cursors(def_id PRIMARY KEY, cutoff, finished_at, rowid, cycle_started_at)`；每轮cutoff固定，next窗口和删除及游标推进同事务。耗尽窗口后结束cycle并清游标，下轮从头检查新超龄/迟到插入，避免永久漏低key。dry_run不持久游标、不写，预览只一窗口且truncated如实表示未检视范围。

API runner不能`deleted==0`就停止，只在window exhausted/stop/budget停止；报告protected为**本cycle已检视保护行数**，不是全库总数；下界标记在未扫描完时true，UI文案同步。最多20批或2秒（先到先停），批间释放Queue锁再检查enabled/cancel；单SQL步不可强制2秒上限，日志区分budget与实际elapsed。

旧`prune_terminal_jobs`作为有界单窗口wrapper保留测试兼容，后台新接口带cursor。latest/tie/NULL/边界保护不变；candidate predicate查询不会为“找可删行”无限穿过protected前缀。

### 5.2 SSE

jobs.db新增单行`job_change_state(id=1, revision, finished_count)`，迁移时一次COUNT初始化；INSERT/UPDATE/DELETE触发器维护finished_at>0的count及revision。所有路径（包括retention/recover/cancel）同事务更新，不靠调用者记得发事件。SSE仍一共享poller、2秒、无订阅停止；只读单行，不重COUNT全history；raw SQL测试也能触发正确计数。新增`revision`不移除旧count字段；retention使count下降也通知。

## 6. 其他并发/协议风险的关闭门槛

- 搜索每请求每Site并发保留，但总准入4、请求deadline12秒；取消后permit由blocking工作持有到结束，不无限生成线程。单Site失败独立报告；停止SSE后不再排新Site。
- fs/browse独立并发2，只读等待10秒；stat/read_dir错误保持原映射，容量不足503；超时不承诺内核IO被取消。
- WebSocket发送timeout5秒，超时关闭连接释放状态；独立周期撤销检查不能被send背压永久阻塞；真实backpressure测试。
- Jobs多连接claim：BEGIN IMMEDIATE包next选择/key检查/UPDATE；两连接Barrier测试相同key仅1running。当前部署仍单实例，不宣称新支持多进程完整服务。
- Subscribe跨库补偿：故障注入并发验证后，将删除新Media补偿改为“无任何Subscribe/ledger引用且当前内容等于本次创建”才删；新增alias恢复需CAS防覆盖。持久缺job reconcile补齐Subscribe def，不盲删另一请求的Media；详列任务，不把跨库原子性当已解决。
- Range：多range416保持，补If-Range日期/ETag合同前先确认需求；本批只加表征测试/文档，不改变流认证。真实客户端依赖未确认时标兼容边界，不假造bug修复。

## 7. 验收与回退

结构门槛：100k无关ledger+100MB无关catalog不影响目标小页的NFO/stat次数（热路径为0）；500k Jobs单批visited≤500；20个protected-only窗口后仍能恢复游标处理后部可删项；慢FS阻塞期间auth/心跳可继续；活跃扫描全局≤2且每库≤1；fixture原输出同语义。

测量：100/1k/10k Movie和TV，每格30样本，冷/热分开；200k ledger和500k Jobs做EQP/VM-step/最大锁持有/内存与响应bytes；结果写新审计目录，不覆盖历史基线。不设跨机器绝对ms硬承诺。

迁移：library8→9；app5→6、catalog1→2、subscribe4→5用于revision；jobs现无user_version，新增元数据表迁移幂等，不擅自设置Store的版本常量。回退必须恢复一致性备份，禁止改小user_version。投影可清重建，ledger/用户事实/scan运行意图不能当cache清空。
