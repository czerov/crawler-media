# 第一批：Library 数据安全与生命周期边界技术设计

- 状态：待用户审阅；本文件不表示已实施或已验收。
- 日期：2026-10-13（Asia/Shanghai）。
- 代码基线：`ba82258`；实施前重新核对工作区，不能覆盖后续用户改动。
- 对应实施计划：[第一批实施计划](<2026-10-13-library-data-safety-batch-1-plan.md>)。
- 依据：[术语](<../../../CONTEXT.md>)、[SQLite 生命周期分区](<../../adr/0008-sqlite-system-of-record.md>)、[Library 删除约定](<../../adr/0012-library-removal.md>)及本轮独立源码审查。

## 1. 目标与批次边界

本批只解决“文件消失/恢复、扫描登记、嵌套归属和索引清理不能误删用户事实”这一组问题。交付后，外部重命名、临时不可达、旧 missing 标记、外层 Library 操作，不得造成 Media、Playback 或其他 Library 的索引被错误删除。

| 本批包含 | 后续批次，不在本批实施 |
|---|---|
| watcher 删除事件改为持久缺失标记 | 播放会话代际、seek 统计 |
| 扫描恢复清 missing；管理员清理前复核 | 自动 probe 失败恢复/outbox |
| verify/prune/full scan/incremental scan/Video scan 的归属边界 | 扫描 Job 化、预算、游标、单飞 |
| 统一同库 ledger 派生事实事务清理 | 分页投影、读连接、catalog/Jobs 性能 |
| 冻结、配置变化、旧 ledger ID 的发布检查 | 自动跨路径身份匹配、改名保持 LedgerId |

不承诺无可靠别名的文件改名后一定复用 MediaId。保留旧用户事实，与安全地重新关联事实是两件事。沿用 ADR-0012 的可靠 NFO/别名规则；禁止仅凭相似文件名迁移 Playback。

## 2. 当前代码事实

| 风险 | 当前位置 | 本批处理 |
|---|---|---|
| 最后文件消失后删除 Playback/Media | [watcher cleanup](<../../../crates/api/src/fs_watcher.rs#L535-L559>) | 移除自动身份/用户事实清理 |
| 重扫已有路径不清 missing | [登记入口](<../../../crates/api/src/watch_ledger.rs#L15-L23>) | 仅清缺失标记，不覆盖已有质量信息 |
| verify/prune 只按 root 前缀匹配 | [Store 文件核验与清理](<../../../crates/store/src/library.rs#L325-L436>) | 使用严格归属快照与提交复核 |
| prune 裸 DELETE 绕过派生清理 | [missing 删除](<../../../crates/store/src/library.rs#L374-L413>) | 复用统一 library.db 事务入口 |
| 增量/Video 没有 full Movie/TV 的 owner filter | [扫描模块](<../../../crates/api/src/http/library_scan.rs>) | 所有登记入口共用范围验证 |

现有 [LibraryOwnership](<../../../crates/store/src/library_ownership.rs>) 的 `owner()` 有 default fallback，不能直接用作删除归属。现有 strict 路径解析会做文件 IO，也不能放入长时间持有 Store 的逐文件循环。

## 3. 必须保持的不变量

- **S1 用户事实保留**：watch、verify、missing-rows 和扫描不会删除 Media、Playback 历史/进度/观看/收藏、collection 关联、Subscribe、pending submission 或 Downloader 配置。
- **S2 字节保留**：本批核验/清理不删除、移动或重写视频、STRM、字幕、NFO、poster、fanart、章节图等磁盘内容；正常 STRM 内容变化仍可使派生缓存失效，但不能由“缺失清理”删除字节。
- **S3 严格范围**：只操作请求 Library 在同 MediaKind 下最长唯一根归属的 ledger；无匹配不回退 default，等深歧义拒绝操作。
- **S4 故障保守**：只有明确 `NotFound` 才是文件缺失；PermissionDenied、NotADirectory、路径解析失败、root 不可读都是 unknown，不可转成 missing 或删除许可。
- **S5 恢复保真**：已有路径恢复保留 LedgerId、MediaId、probe 质量和 filter_score；只清 `missing_at`。STRM 同 URL 恢复保留缓存；URL 改变沿用有测试的失效规则。
- **S6 提交有效**：提交前检查 Library 仍存在且未冻结、配置未变化、目标 ledger ID/path/media_id 未替换。旧观测不得删除同路径新建的 ledger。
- **S7 同库原子**：ledger 与文件派生行在 library.db 的一个 Immediate 事务内删除；失败整笔回滚，不声称跨库原子。
- **S8 已运行工作**：清理前取消目标 ledger 的 probe 工作；提交后旧 worker 不得重新发布已删除 ledger 的派生事实。

## 4. 行为决策

### 4.1 watcher：缺失是状态，不是自动永久清理授权

所有视频格式和 STRM 都采用以下状态机，复用现有 `ledger.missing_at`，不新增 schema：

```text
在位 --root健康且路径明确NotFound--> 缺失（只在首次观察写missing_at）
缺失 --确认路径重新在位--> 在位（清missing_at）
任意 --root不可读/路径观察失败--> 保持原状态 + warn/error
缺失 --管理员清理 + 当前再次确认缺失 + 提交有效--> 删除索引及文件派生行
```

**取消 watcher 自动删除 ledger 的行为，包括 STRM 45 秒到期删除。** 现有 `StrmGraceTracker` 保留对外名字与 `set_now` 等测试能力，继续承担同 URL 重建/变更识别；pending 到期只可结束内存防抖，不能调用 ledger 删除。生产 5 秒删除 sweep 移除或改为纯 tracker 内存维护，不新建自动 GC。

这是有意兼容性变更：真实删除文件后，缺失索引保留至管理员执行 missing-rows；UI 可继续显示 missing。收益是不用猜测“删除、改名、挂载失联”究竟哪一种。现有“45 秒后必删 ledger/重建新 LedgerId”测试必须改成新的业务断言，不能简单删测试。

事件只影响唯一归属、`realtime_watch=true` 且未冻结的 Library；目录事件只检查其下候选行，不影响同前缀兄弟目录。现有 intake/Transfer/Scrape 提前调度保持不变。移除 cleanup 中删除 Media、Playback、collection、pending、共享 marker 的分支。

### 4.2 verify 与 missing-rows 的 HTTP 合同

路由及管理员权限不变：

- `POST /api/v1/libraries/{id}/verify`
- `DELETE /api/v1/libraries/{id}/missing-rows[?media_item_id=<UUID>]`

成功字段保持兼容，并新增可读诊断：

```json
{"ok":true,"data":{"missing":2,"restored":1,"skipped":0}}
{"ok":true,"data":{"deleted":2,"restored":1,"skipped":0}}
```

`skipped` 是提交时 ledger 已改变或单行观察为 unknown 的数量；`restored` 是确实清除旧标记的行数；`deleted` 是实际删除行数。重复执行成功且计数为 0。仅按 `media_item_id` 清理必须同时满足 Library 归属，不能把该 Media 在别库的其他文件包含进来。

| 条件 | 响应/动作 |
|---|---|
| 无 Library | 404 `library.missing` |
| 非管理员 | 现有中间件 403 |
| Library 冻结/删除中 | 409 `library.deleting` |
| 根归属歧义 | 409 `library.ownership_ambiguous`；该请求不改 ledger |
| 请求 Library 任一 root 无法读取或不再是目录 | 409 `library.root_unavailable`；该请求不改 ledger |
| 快照后 Library 配置改变 | 409 `library.scope_changed`；该请求不改 ledger |
| 单文件 PermissionDenied/其他 IO 错误 | 保留行及标记；`skipped += 1`，warn 含路径/错误 |
| 旧 missing 文件恢复 | 清标记，不删 ledger；`restored += 1` |
| DB/worker 错误 | 500 固定 message；完整上下文只进日志 |

root 健康最低准入：解析成功、metadata 确认目录、read_dir 成功建立读取且取得首项或空目录结束，无读取错误。**这不能证明 NAS 挂载身份正确**：卸载后仍存在的空 mountpoint 可能通过检查。本批通过 watcher 不自动删索引/历史、管理员显式确认减少破坏；不伪称解决空挂载点检测。mount ID、sentinel 或 NAS 专用探测属于后续独立设计。

### 4.3 观察与提交分离

流程固定为：

1. 锁内取 Library 配置、removal 状态、ledger/media owned 快照。
2. 释放 Store；在 bounded blocking 作业内解析根、路径和文件状态。
3. 取得工作租约时遵守 **Registry → Store**；禁止持 Store 再 acquire lease。
4. 清理作业取消目标 probe，再于 lease 的 publication barrier 中拿 Store，核对配置和 ledger 身份。
5. 用纯内存严格归属判定复核；单个 library.db 事务写标记/删除派生行。

verify 和 prune 使用 `http_work.run_to_completion`，不使用会“超时返回但后台继续删除”的 `run(deadline, ...)`。请求取消后，已被准入的 owned 作业仍执行最终检查；容量不足返回 503 `runtime.busy`。文件系统 IO 没有可移植强制中断保证，因此本批不许宣传无限 NAS 挂起已解决。

提交前对拟删除文件再观察一次。仍存在“最后一次 stat 后外部程序立即恢复文件”的 OS 竞态，SQLite 无法和外部文件系统组成事务；本批保证不删除磁盘字节与用户事实，并避免旧快照删除新 LedgerId，不承诺完全消除外部 TOCTOU。

### 4.4 所有扫描登记采用同一归属边界

全量 Movie/TV、增量 Movie/TV、全量 Video、增量 Video 四种路径都必须：

- 在发布前筛选当前请求 Library 的严格归属；别库文件直接排除，不为它做识别、Scrape、probe 或封面副作用。
- 同 MediaKind 的嵌套根仍按最长唯一规则；不同 MediaKind 的重叠根沿用当前规则，不在本批重新定义跨 kind 所有权。
- 不跟随子项软链接；配置 root 自身的软链接先按现有严格 resolver 规范化，不能用词法 fallback 绕开解析错误。
- 在 `lease.publish` 中检查 Library 活跃和捕获配置相等；配置或规范化路径失效则拒绝本轮发布，不能默默发布部分越界行。
- 已登记且确认在位的路径，调用 `clear_ledger_missing`；不得重建 row 或覆盖探测质量。恢复的旧行不加入“新插入”集合，避免普通重扫重复 probe。

本批不把全扫描改成长期 Job，也不解决部分提交后补 probe；那是第二批独立生命周期问题。

### 4.5 文件派生事实清理

建立一个内部事务删除入口，供 `delete_ledger_path`、`delete_ledger_for_media` 和新的 missing prune 复用。以删除前核对的 LedgerId 为目标，不再裸按 path 删除。

| 数据 | 本批删除索引时行为 |
|---|---|
| ledger | 删除目标行 |
| file_meta（含缓存章节字段） | 同事务删除 |
| fingerprint_cache / fingerprint_attempts / probe_stage_state / fingerprint_samples | 沿用并复用现有清理 helper，同事务删除 |
| scrape_sidecars | 删除数据库关联，不删除 sidecar 字节 |
| probe_job_units / probe_jobs | 保留执行历史；活跃目标先取消，不能留下 active 单元；不把历史执行行误当缓存孤儿 |
| Media、Playback、collection、Subscribe/pending | 不删除、不改写 |
| media_markers / 共享 fingerprint model | 本批保守保留共享事实；不得按 Media 粗粒度删除，也不得手写破坏 model 关联的级联 |

不全库批量回收既有孤儿；这需要另行确定范围与预算。事务清理出现错误时整个 library.db 删除回滚；之前已取消的 probe 保持取消，可由现有手动 probe 重试，不回滚外部进程取消动作。

## 5. 实现边界与文件分解

不增加第三方依赖、不增加数据库版本、不迁移 Playback、不改公开图片权限。新增 focused 模块，避免往接近 800 行的扫描模块继续堆逻辑：

- Store：规范化严格 scope、带身份条件的缺失写入与事务删除。
- API：文件观察 trait/真实实现、verify/prune 协调、watcher 缺失协调、scan 归属发布 helper。
- 测试：Store 原子性与所有权；API HTTP 合同；公开 watcher event seam；full/incremental/Video 的登记与冻结竞态。

纯 ownership matcher 可以复用，但必须有 strict 方法，不能改变旧 browse 的 default fallback。新 scope 在 Store 锁外构造；其原始配置副本用于提交时比较。Library 的 path/kind/root 及 blocked 状态变化会使 scope 过期。

## 6. 验收矩阵

| 编号 | 必须可观察的结果 | 实施任务 |
|---|---|---|
| A1 | 最后 mkv/STRM 消失、经过 45 秒及重启：Media、两名 User 的位置/收藏/历史、collection 仍在 | T4 |
| A2 | root 不可达或 PermissionDenied：不误标缺失、不删除现存标记 | T1/T3/T4 |
| A3 | verify→恢复→scan→prune：LedgerId 不变、missing 清零、deleted=0 | T3/T5 |
| A4 | 外层 verify/prune/scan 不改变内层 Library 的 ledger/缓存/sidecar | T1/T3/T5 |
| A5 | 相同深度双根歧义、路径前缀兄弟、缺失祖先、软链接路径均保守处理 | T1/T3/T5 |
| A6 | 清理过程中恢复文件/替换同路径 LedgerId/修改root/冻结：不按旧观察删除或发布 | T3/T5 |
| A7 | 删除派生中途 SQLite failure：ledger 和派生行一起回滚，别库行不变 | T2/T3 |
| A8 | STRM 同 URL 恢复保留缓存；URL 改变只使对应派生事实失效 | T4 |
| A9 | HTTP 权限、响应 envelope、原成功字段和重复请求兼容 | T3 |
| A10 | 旧 probe 正在运行时清理，取消后不得复活已删派生行 | T3/T6 |

## 7. 发布与回退

1. 本批无 schema 变更，先备份四个 Store 文件和 jobs.db（使用现有一致性备份方式，不在线直接复制 WAL 主文件冒充完整备份）。
2. 发布说明明确“watch 不再自动删缺失索引，管理员确认清理”。已有 missing 数据不自动清空，也不在启动时批量删除。
3. 验证后观察 root_unavailable、scope_changed、skipped、missing/restored/deleted 计数，不新增每文件 info 日志风暴。
4. 若回退旧二进制，自动删除用户历史的旧风险会回来；不能承诺历史自动恢复。事故处理先停 watcher、保留数据库和日志，再按备份恢复，不通过删 Media 或重扫冒充修复。

## 8. 验证声明

本文件是设计，不是测试报告。实施必须按配套计划先红后绿；全部功能验收通过后才可把状态改为“已实施”。本轮不承诺真实 NAS、浏览器或 VidHub 已验证。
