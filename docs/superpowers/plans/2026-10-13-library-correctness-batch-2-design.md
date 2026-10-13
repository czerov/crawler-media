# 第二批：Playback 正确性与扫描失败恢复技术设计

- 状态：待审阅，未实施；基线 `ba82258`，开发时以第一批完成后的接口为准。
- 前置：[第一批技术设计](<2026-10-13-library-data-safety-batch-1-design.md>)、[第一批计划](<2026-10-13-library-data-safety-batch-1-plan.md>)。
- 配套：[第二批实施计划](<2026-10-13-library-correctness-batch-2-plan.md>)。

## 1. 范围

本批闭合三组独立可验收行为：B2-P 播放事件代际；B2-W 实际观看时长；B2-S 扫描结果完整性与探测补偿。第三批才负责长期扫描 Job、单飞/预算、浏览和维护性能。

必须保持第一批S1–S8：不得为了失败恢复删除Media/Playback，不得取消严格owner/lease，恢复旧path只清missing、不覆盖可靠探测数据。只读图片路由保持public；Jellyfin流路由保持协议认证。

## 2. 播放事件代际

### 2.1 现状与目标

当前 [协议事件DTO](<../../../crates/media-server/src/routes/sessions.rs>) 不接收PlaySessionId；[provider](<../../../crates/api/src/media_server_provider/playback.rs#L204-L221>) 对任意不同target事件关闭当前会话。目标：A→B后迟到A Progress/Stopped不得动B；相同文件不同播放代际也必须可区分。

不把PlaySessionId当认证凭证。身份仍来自已有User/token/设备授权；ID只限定事件属于哪次播放。

### 2.2 持久化与迁移

subscribe.db schema从 **3→4**（第一批无迁移）；新增：

```sql
ALTER TABLE playback_sessions ADD COLUMN play_session_id TEXT;
ALTER TABLE playback_sessions ADD COLUMN last_report_at_ms INTEGER NOT NULL DEFAULT 0;
ALTER TABLE playback_sessions ADD COLUMN last_event_sequence INTEGER;
CREATE TABLE playback_session_ids (
  user_id TEXT NOT NULL, device_id TEXT NOT NULL, play_session_id TEXT NOT NULL,
  item_id TEXT NOT NULL, issued_at INTEGER NOT NULL, generation INTEGER NOT NULL,
  state TEXT NOT NULL, PRIMARY KEY(user_id, device_id, play_session_id)
);
CREATE INDEX playback_session_ids_expiry_idx ON playback_session_ids(issued_at);
```

`state`仅pending/active/closed；item_id存规范化LedgerId字符串，不用MediaId替代文件身份。generation在同User/Device签发事务内取MAX+1，清理保留当前active，避免同秒签发不能区分先后的问题；激活只允许尚未被更高generation激活取代的ID。清除过期ID的截止为**30天**，只删closed或pending，不删当前active；每次签发最多清500条，第三批可复用维护预算。不得因为ID表过期撤销登录token。

SessionRow增加三个对应字段；旧row play_session_id=None，last_report_at_ms回填last_report_at*1000（溢出夹紧）。所有构造/映射/SQL及fixture同一任务更新；旧位置、观看、收藏、日志不重算。不另起第二份Playback权威表。

### 2.3 协议合同

- PlaybackInfo在可见LedgerId和设备确定后签发UUID，写pending记录，再返回该PlaySessionId。设备规则复用现有认证解析，不拿token充当设备ID。
- Jellyfin事件接收可选`PlaySessionId`、可选`EventSequence`（本地扩展，不要求第三方发送）。自有progress接收可选`play_session_id`、`event_sequence`；自有start响应新增play_session_id，前端后续原样回传。
- 非空session ID最大128字节；非UUID并非必拒，但必须是服务端签发的匹配记录。超长/sequence负数400；未知/closed/不同User或Device或LedgerId的ID事件**忽略并返回原协议成功形状**（Jellyfin204，自有ok），记录脱敏debug，不泄露他人会话存在性。
- Playing/start可激活pending并关闭同设备旧active；重复同ID Playing不增加play_count、不清watched_ms。
- Progress/Stopped只能更新匹配当前ID、User、Device、LedgerId的active。Stopped关闭后重复Stopped无副作用；旧pending/closed ID的Playing不能重新复活旧代际。pending激活时将同设备更低generation的pending标closed（不能仅按秒时间比较）；晚到旧Playing不可替换新会话。
- 有EventSequence时只接受严格递增；无sequence时无法证明同代际心跳的发送先后，不声称完全解决同会话乱序。位置允许合法后退，不采用“只能递增”的错误防乱序方案。

### 2.4 无ID客户端兼容

不能要求所有VidHub/Infuse已发送PlaySessionId：

| 输入 | 兼容行为 |
|---|---|
| 无ID Playing | 创建legacy会话；可替换当前target，重复同target不重复计数 |
| 无ID Progress/Stopped，当前legacy且LedgerId相同 | 正常更新/关闭 |
| 无ID Progress/Stopped，当前target不同或没有会话 | 忽略，不创建、不覆盖位置、不关闭其他target |
| 无ID事件，当前为有ID会话 | 仅允许同LedgerId兼容更新，无法区分同文件代际；日志标legacy_fallback |

这能防跨target迟到事件，不能完美区分无ID的“同文件A第二次播放”。此兼容限制在发布说明保留；不破坏已登录token/MediaBrowser认证格式。

## 3. 实际观看时长

目前 [Jellyfin](<../../../crates/api/src/media_server_provider/playback.rs#L261-L270>) 与 [自有心跳](<../../../crates/api/src/http/playback/heartbeat.rs#L148-L181>) 都累加位置正差；统一为共享纯策略。

输入：之前SessionRow、当前position、paused、event、服务端毫秒时间、可选播放倍率；本批只采纳**0.25..=4.0**的有限倍率，缺省1.0，非法值400；未传倍率不是对第三方倍率支持的承诺。

- `elapsed = clamp(now_ms - last_report_at_ms, 0, 30_000)`；旧时间/重复毫秒不累计。
- 前一状态paused或本次暂停事件：delta=0；Stopped本身不被强制当paused，可累计停止前最后一段。
- `advance=max(position-previous.position,0)`。
- `advance > elapsed*rate + 2_000`视为seek，本区间delta=0。
- 否则`delta=min(elapsed, advance/rate)`，向下取整毫秒；watched_ms用saturating_add。
- 后退、重复位置、长时间掉线后大跳转均不虚增。这里统计的是保守的实际观看墙钟，不是内容覆盖长度，不改90%/最后30秒的“已观看”判定。

用注入Clock测试，不调用sleep。不回写旧日志，不“修正”已存在统计；新旧口径以发布版本说明。

## 4. 扫描恢复与结果报告

### 4.1 部分成功必须可见

当前 [扫描入口](<../../../crates/api/src/http/library_scan.rs>) 缺root跳过、Video吞读目录错误且只返回queued。新增响应字段，保留原queued/transferred字段：

```json
{"ok":true,"data":{"queued":true,"transferred":true,"status":"partial",
 "scanned":120,"inserted":10,"restored":2,"probe_pending":10,
 "roots":[{"path":"/media/movies","status":"completed","visited":120,"errors":0}],
 "error_count":1,"errors_truncated":false}}
```

本批同步入口`queued`继续表示后续probe工作，不伪称扫描已进入durable Job。status为completed/partial；全root不可达返回409 library.root_unavailable；DB/身份冲突返回500 library.scan_failed，附已提交计数，不把前缀提交宣称回滚。每root的unreachable/partial/completed独立报告；最多返回20条脱敏错误摘要，完整path/error结构化日志。文件消失NotFound为skipped，PermissionDenied为error，读目录失败不能completed。

### 4.2 durable probe obligation

library.db schema **7→8**，新增 `ledger_probe_obligations(ledger_id TEXT PRIMARY KEY, created_at_ms INTEGER NOT NULL, reason TEXT NOT NULL)`；reason本批只用scan。它是待处理义务，不是第二份probe状态。

新增 `Store::insert_scanned_ledger(&self, row: &LedgerRow, now_ms: i64) -> Result<(), StoreError>`：在同一个library.db Immediate事务中写ledger和obligation。Media身份仍在app.db，不声称跨库原子。沿用existing-path不覆盖规则；新增Video同样写义务。

- 扫描逐项失败/元数据重绑失败后，已提交ledger仍有义务。
- dispatcher在启动及每5秒取最多100条，锁外请求现有probe；只在现有持久probe job成功创建或确认为已active/已有可靠完成结果后ACK。
- 崩溃在enqueue之后ACK之前：依现有active scope唯一约束与完成状态去重，不重复排完成任务。
- 冻结Library保留或取消义务按第一批生命周期：清理ledger同时删除其obligation；worker不得给不存在ledger补任务。新义务失败保留并log，不因一次错误永久丢失。
- 同URL旧path恢复不创建新obligation；只补本次新增ledger未完成的义务，不强制重探所有历史行。
- 第一批统一事务清理helper追加obligation删除；此表是第二批新增，所以第一批计划不预先操作不存在表。

### 4.3 格式一致性

Video不再维护第二份扩展名常量，统一调用`library::is_video_file`。mts/mpg/mpeg/vob/iso与Movie/TV识别入口一致。支持格式表示允许登记，不代表当前ffmpeg一定能完整探测每种容器；探测错误按现有状态报告。

## 5. 数据/接口衔接

| 第一批输出 | 第二批使用方式 | 第三批承接 |
|---|---|---|
| LibraryPathScope/ScanScope | 结果报告和probe义务只针对过滤后目标 | 可取消chunk扫描仍用同scope |
| delete_ledger_in_tx | 增加obligation清理 | 增加浏览派生失效 |
| LibraryFileRead | root准入/失败测试 | 不替代受控walker |
| missing恢复规则 | 不把restored计为inserted | 缓存状态刷新 |
| lease.publish/身份复核 | probe发布和播放target检查保留 | durable scan恢复重新拿lease |

## 6. 验收

1. A→B→late A Stop/Progress/Playing，B不关闭，A位置不被旧事件覆盖；同文件不同ID亦然。
2. ID绑定User/Device/LedgerId；重复Playing/Stopped幂等；有序sequence拒绝旧值；无ID兼容场景照表执行。
3. t=1000ms start0，t=2000ms seek600000：watched=0；之后正常1s推进累计1000；暂停/回拖/掉线/2倍速精确验证。
4. 扫描提交前缀后触发alias冲突，重启dispatcher仍为前缀创建probe；enqueue成功ACK前崩溃不重复执行完成任务。
5. 部分root失败、Video目录读取错误、文件stat权限错误不能报告completed。
6. 格式登记一致；子项symlink依第一批仍跳过。
7. 升级旧subscribe/library库幂等，保留所有旧用户事实；较新版本数据库仍拒绝降级打开。

## 7. 回退与限制

含schema升级，旧二进制可能因版本预检拒绝打开；必须保留升级前一致性备份，不直接把user_version改小。无真实客户端抓包时，PlaySessionId支持只算协议测试通过；VidHub/Infuse验收另列Not run。故障注入用临时数据库/假网络/可控probe，禁止碰真实Library。
