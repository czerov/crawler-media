# 数据库、接口与全库扫描：三批连续开发总路线图

- 文档状态：全套设计/开发计划已编写，**等待整体审阅，不表示已实施**。
- 审查源码基线：`ba82258`；第一批文档提交：`5f98bda`。后续实现从当前分支最新HEAD核对，不reset到旧版本。
- 用户目标：先准备全部批次，再按依赖连续开发；**不需要每完成一批重新补写下一批设计**。
- 日期：2026-10-13，Asia/Shanghai。

## 1. 文档导航与阅读顺序

| 顺序 | 技术设计 | 开发计划 | 主要交付 |
|---|---|---|---|
| 1 | [第一批设计](<2026-10-13-library-data-safety-batch-1-design.md>) | [第一批计划](<2026-10-13-library-data-safety-batch-1-plan.md>) | 用户事实保留、missing恢复、安全清理、统一归属 |
| 2 | [第二批设计](<2026-10-13-library-correctness-batch-2-design.md>) | [第二批计划](<2026-10-13-library-correctness-batch-2-plan.md>) | 播放代际/观看时长、扫描partial与probe恢复 |
| 3 | [第三批设计](<2026-10-13-library-performance-batch-3-design.md>) | [第三批计划](<2026-10-13-library-performance-batch-3-plan.md>) | 浏览投影/有界IO、扫描operation、Jobs预算、并发与规模验收 |

只批准文档不等于批准代码实施。整体审阅后选定执行方式即可连续执行；每批结束跑gate并输出结果，再进入下一批，不再就已明确范围重复问“下一批要不要写”。出现新的产品行为决策、不可验证前提或本计划外schema冲突时才暂停确认。

## 2. 三批范围与依赖

```text
B1.T1 strict scope
  → B1.T2 atomic ledger cleanup
  → B1.T3 scoped verify/prune
  → B1.T4 watcher missing-only
  → B1.T5 scan publication/restoration
  → B1.T6 safety gate
      → B2.T1 session IDs/schema
      → B2.T2 protocol generation
      → B2.T3 watch time
      → B2.T4 durable probe obligation
      → B2.T5 scan reports/formats
      → B2.T6 correctness gate
          → B3.T0 release baseline
          → B3.T1 projection/revisions
          → B3.T2 builder/invalidation
          → B3.T3 browse paging
          → B3.T4 visible scope/offlock IO
          → B3.T5 scan operations/202
          → B3.T6 bounded walker/checkpoint
          → B3.T7 retention window/cursor
          → B3.T8 SSE change state
          → B3.T9A admission/WS
          → B3.T9B claim transaction
          → B3.T9C compensation/range boundary
          → B3.T10 whole acceptance
```

执行默认串行，避免多个任务同时改schema、ApiState和公共trait。允许研究/审查并行，不默认并行写共享文件。任何实施模式均先读当前文件、看用户diff，不能覆盖预存未跟踪计划。

## 3. 跨批次稳定接口契约

| 契约 | 首次建立 | 后批允许的扩展 | 禁止的回退 |
|---|---|---|---|
| LibraryPathScope/StrictOwner | B1.T1 | B3投影/scan复用同规则 | default fallback用于删除/发布 |
| delete_ledger_in_tx | B1.T2 | B2义务、B3投影派生清理 | 裸path DELETE、级联删用户历史 |
| LibraryFileRead + presence observations | B1.T3 | B2结果诊断、B3刷新 | 把PermissionDenied当Missing |
| watcher missing-only | B1.T4 | B3标dirty/合并增量扫描 | 45秒后自动删Media/Playback |
| ScanScope/lease publication | B1.T5 | B3每chunk复核 | 仅持外层lease写内层库 |
| SessionRow/session binding | B2.T1–T2 | 新字段必须同步mapper/fixtures | 迟到Progress/Stopped替换当前target |
| watched_increment | B2.T3 | 更精确测量需另审计口径 | 直接累加seek位置差 |
| ledger_probe_obligations | B2.T4 | B3chunk同事务写入 | 只靠本轮inserted内存数组保证probe |
| ScanReport | B2.T5 | B3operation状态承载同报告 | 把partial/unreachable报completed |
| browse generation | B3.T1–T3 | 可重建cache，权威配置实时授权 | 当第二份Media/ledger权威事实 |
| retention window | B3.T7 | visited/保护下界口径 | LIMIT输出数冒充实际检视预算 |

**发布屏障的typed错误**：现有`LibraryWorkLease::publish`外层返回String，不为方便丢失错误类型。协调器可返回嵌套`Result`，外层cancel映射Deleting、内层typed scope/DB错误按合同；不要用error字符串contains决定状态码。

**chunk事务复用**：B2 `insert_scanned_ledger`为公共单row事务；B3需提取内部`insert_scanned_ledger_in_tx`供chunk使用，ledger+obligation+seen同事务。不能在外层事务再调用无条件BEGIN的公共接口形成嵌套事务失败。

## 4. schema版本与迁移所有权

| 数据库 | 当前基线 | 第一批后 | 第二批后 | 第三批后 | 变更 |
|---|---:|---:|---:|---:|---|
| app.db | 5 | 5 | 5 | 6 | 浏览相关revision |
| catalog.db | 1 | 1 | 1 | 2 | revision、定向读取支持 |
| library.db | 7 | 7 | 8 | 9 | B2 probe义务；B3投影/scan operation/revision |
| subscribe.db | 3 | 3 | 4 | 5 | B2会话ID/毫秒时间；B3revision |
| jobs.db | 无现成user_version合同 | 不变 | 不变 | 幂等新增表/索引/trigger | retention cursor、change state |

表中是当前基线下的计划版本。若实施时别人已新增迁移，必须重新分配下一个版本、更新本表及相关设计，不能复用覆盖。Store只读高版本拒绝打开保护保持。无schema跨库原子承诺；每库迁移成功才更新版本，失败不标成功。

B3在同一library版本9内分任务实现模块时，每个中间提交必须能从8幂等升级且不丢数据；最终fresh/upgrade/reopen三路径schema等价测试覆盖完整表集。版本9的中间开发二进制不作为可回退生产版本。

## 5. 用户可见行为变化与前端衔接

| 时间点 | 变化 | 客户端处理 |
|---|---|---|
| B1 | watcher不再自动删除缺失索引/历史 | 管理员继续使用missing-rows确认清理；文件恢复自动清标记 |
| B1 | verify/prune新增restored/skipped与409诊断 | 原计数字段保留，错误按envelope显示 |
| B2 | PlaybackInfo/start返回真实绑定会话ID | Web透传；第三方无ID按legacy兼容，不强制重新登录 |
| B2 | watched_ms改为保守墙钟累计 | 仅新事件，旧历史不重算 |
| B2 | scan返回completed/partial及root报告 | 同步扫描合同暂留，queued仅指probe |
| B3 | POST scan改202+operation_id | 同提交修改Web API及调用方，轮询operation，partial不是成功完成 |
| B3 | 浏览文件facts后台刷新 | 外部修改可能60秒+排队延迟；权限/冻结不能stale |
| B3 | retention protected为检视窗口/周期保护下界 | 同提交改Web类型/summary，禁止显示为全库精确总数 |

## 6. 本轮审查问题覆盖矩阵

| 问题/风险 | 落地任务 | 验收要点 |
|---|---|---|
| watcher最后文件误删身份/用户历史 | B1.T4 | 无Subscribe/最后文件/重启仍保留两User事实 |
| 恢复后missing未清、prune误删 | B1.T3/T5 | verify→恢复→scan→prune LedgerId不变 |
| verify/prune侵入嵌套库 | B1.T1/T3 | strict最长唯一owner，不只前缀 |
| 增量/Video归属与冻结发布 | B1.T5/T6 | 四入口、嵌套、freeze/改root竞态 |
| 派生孤儿/清理回滚 | B1.T2/T6 | 目标派生同事务删、其他库/共享事实留 |
| 迟到播放事件/同文件代际 | B2.T1/T2 | ID绑定/legacy/幂等/sequence |
| seek观看统计虚高 | B2.T3 | 假时钟精确断言两协议 |
| 扫描部分提交后永远漏probe | B2.T4 | ledger+义务原子，enqueue/ACK kill点 |
| 根不可达/Video吞错误假成功 | B2.T5 | partial/unreachable/root报告 |
| Video支持格式不一致 | B2.T5 | 一套is_video_file |
| 全实例cache/ledger/units读取，小页线性成本 | B3.T1–T4 | 定向facts/批量units、热路径IO0 |
| gallery分页前全量图片IO | B3.T3 | 投影剔空后分页，只渲染当前页 |
| Library stats/cover/NFO锁内IO | B3.T2/T4 | gate阻塞磁盘时auth/心跳继续 |
| scan长锁/逐row重复owner/全量内存 | B3.T5/T6 | scope快照/100chunk/迭代walker |
| scan并发无界/遍历不能取消/断连继续 | B3.T5/T6 | durable operation、2全局/1每库、取消诚实边界 |
| retention反复全历史扫描 | B3.T7 | 原始500visited、cursor、protected-only不饿死 |
| SSE每2秒COUNT全终态 | B3.T8 | 触发器维护真实count/revision，单行poll |
| 搜索/目录spawn_blocking准入缺口 | B3.T9A | 独立4/2预算，timeout permit不提前释放 |
| STRM整文件同步读取 | B3.T4 | 64KiB上限/offlock、慢IO边界 |
| WS发送阻塞撤销检查 | B3.T9A | 5秒send deadline/背压真实socket测试 |
| 多连接claim竞争（条件风险） | B3.T9B | 两连接公共Queue测试，确认后修Immediate |
| Subscribe跨库补偿竞争（条件风险） | B3.T9C | 故障注入证据、CAS引用检查、缺job补齐 |
| Range多段/If-Range（兼容边界） | B3.T9C | 表征测试与文档；未确认需求不擅改协议 |
| Release性能/规模证据不足 | B3.T0/T10 | 同机器30样本、200kledger/500kJobs、status/bytes |

范围解释：此矩阵对应本轮从0审查报告，不把此前已修复的queued payload、维护停用边界、开发tick行为重复列为新任务。维护配置payload-only CAS/周期重算等历史遗留若未在本轮报告展开，单列后续backlog，不混称三批已覆盖。全项目所有业务接口的完备正确性不能由本计划担保。

## 7. 每批结束gate与停止条件

### B1 Gate

- 第一批A1–A10公共回归全绿；Store/API各自测试与workspace通过。
- watcher最后文件不删用户事实；恢复+清理+嵌套+probe防复活必须实际验证。
- 无schema迁移；文档/测试记录watch兼容性变化。

### B2 Gate

- schema4/8 fresh+upgrade+reopen测试；会话代际、legacy、seek、义务重启、partial格式全绿。
- 涉及store/library/media-server/api逐crate与workspace通过；Web变更typecheck/单测。
- 没有真实VidHub/Infuse就写Not run，不声称设备已验收。

### B3 Gate

- 结构预算、scope/权限、失效、重启、retention、SSE/claim/补偿/背压测试全绿。
- Release相对门槛：items/gallery/history中位至少改善30%、jobs不退化>20%；不达不能静默宣布性能完成。
- 审计必须区分冷/热、大小/模式、响应bytes、HTTP状态、最大锁持有、VMstep/EQP。
- 所有触及crate独立测试+workspace，Web变更验证，代码尺寸/diff检查。

失败时：先保留复现与红测试、修当前任务，不跳到下一批。新产品决定/跨批接口矛盾/schema冲突需要重新审阅具体变更；普通测试失败不算必须问用户的阻塞。无真实NAS/设备只阻塞对应外部验收，不伪造结果，也不因此偷偷删必要代码测试。

## 8. 执行方式与提交策略

- 推荐subagent逐任务实施+新上下文复核；共享接口任务按上方顺序串行，父代理最终审查diff与测试。不是Agent Teams授权。
- Native也可按同计划执行，每批末独立复核；两者都不可跳过红绿和公共seam测试。
- 每任务提交只含自身源码/测试/对应文档；第三批9A/9B/9C分提交。无GitHub issue创建要求。
- 开发前`pwd/git status`确认工作区，记录真实HEAD，读AGENTS/相关ADR；不修改预存未跟踪文件。
- 不要求新worktree，遵守当前分支提交；若用户另选隔离目录再安排。

## 9. 交付清单与状态

| 交付 | 当前状态 |
|---|---|
| 三批设计+三批实施计划+总路线图 | 已编写，待整体审阅 |
| 三批业务代码 | 未开始 |
| 计划中回归/故障注入/性能验证 | 未执行 |
| NAS/浏览器/VidHub/Infuse | Not run |

文档编写只验证本地链接/结构/范围，不运行业务测试冒充实施成果。用户整体确认后可从B1.T1连续开发至B3.T10，不需要再逐批申请“生成下一份计划”。
