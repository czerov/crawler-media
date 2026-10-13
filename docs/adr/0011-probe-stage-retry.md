# 0011: 探测阶段状态与退避持久化 (Probe Stage Retry & State)

## 上下文

现有系统的媒体探测与声纹分析分为两个工作队列（高优先级 `metadata` 与低优先级 `fingerprint`）。
之前，当单集（如第 8 集）片尾提取因为网络短暂 403 失败，但片头提取成功时，
由于缺乏针对子阶段（metadata, intro, outro）细粒度的持久化状态，会导致：
1. 声纹任务标记为整体完成或部分完成后无退避机制；
2. 前端请求或播放器探测再次发现缺 outro 缓存，重复创建 `media_probe` 任务并重新探测整条流水线；
3. 短时间内无限制重试，产生大量日志与无效拉流。

## 决策

1. **按阶段跟踪状态**：在 `library.db` 新增 `probe_stage_state` 表，以 `(ledger_id, context_key, stage)` 为联合主键，分别记录 `metadata`、`intro`、`outro` 的运行/成功/失败状态。
2. **持久化退避策略**：失败时记录 `failure_count` 与 `next_retry_at_ms`（重试间隔：60s, 300s, 900s, 3600s，最多 4 次自动重试，之后需手动触发）。
3. **支持 Partial 终态**：在 `probe_jobs` 表支持 `partial` 终态（片头成功但片尾未就绪），避免当作无退避的失败或整体成功。
4. **生命周期与 Ledger 绑定**：当删除 Ledger 时，同步级联删除对应的 `probe_stage_state`。
5. **轻量对比摘要**：新增 `probe_season_comparison_history` 表记录整季声纹对比摘要，供后续跳过未变化的声纹比对。
## 扫描入库探测义务与重启恢复 (2026-10-13 Batch 2)

在 `library.db` 新增 `ledger_probe_obligations` 表，随扫描台账在同一个 Immediate 事务中原子写入。

- 即使扫描在元数据反查或部分阶段中途退出，已登记的 ledger 仍具备未完成的 probe 义务。
- 调度器每 5 秒自动派发最多 100 条待处理义务；成功安排持久探测任务后才 ACK 删除义务行。
- Ledger 删除级联清理对应的 probe obligation，保证已删除项目不会被异步 worker 恢复。
