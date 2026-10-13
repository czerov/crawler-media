# 下载器任务身份与 Transmission 安全收口 Implementation Plan

> **历史状态（2026-10-13 整理）：** Downloader 身份/409 握手历史方案；当时不支持快照的描述不是永久能力声明，10-05 follow-up 已记录 Transmission 快照实现。
>
> 本文保留原设计、发现/任务 ID 与当时证据；日期本身不表示过时。旧执行/提交/清理命令仅作历史记录，不应直接重跑；复选框不是当前实现清单。当前依据：[数据库/API review](<../../audits/2026-10-13-database-api-review.md>) 与 [剩余计划](<2026-10-13-database-api-followup.md>)，未覆盖的旧 ID 仍须单独对账。

> **规模规则更新：** 文中的 60 行硬限/整改目标是历史要求；当前 [AGENTS.md](<../../../AGENTS.md>) 为生产函数软限 60、测试软限 80、两者硬限 120。历史测试/行数记录不改写，规模合规不等于功能安全。

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 消除相同体积种子的误匹配/误删/误 Transfer；让不支持快照的下载器不能触发 pending 清理；让标准 Transmission 409 会话握手可用。

**Architecture:** 将“找到同一个 Torrent”与“只知道体积相同”严格分离。qB 加入/查文件/删除必须复用安全标题+可选体积匹配；Transmission 移除只删除唯一匹配任务。快照接口将“不支持”与“空且可达”区分；任务列表只有确实成功枚举的下载器才有自动清理资格。409 直接读取第一次响应的 session header，单次带 token 重试。

**Tech Stack:** Rust 2024、ureq 3、Axum 0.8、注入本机 fake HTTP 服务（无需真实下载器）。

## Global Constraints

- `AGENTS.md`：`.rs` 文件硬限 800 行、函数硬限 60 行（测试逻辑不豁免）；`domain` 只放类型，`store` 只持久化；新增测试无真实网络、无 Chromium。
- 不修改自有 REST `/api/v1` 信封、Jellyfin 路由或 schema；静态图片仍公开。
- 文件操作不可仅因同字节体积而判定 Torrent 一致；无法确认身份时保留 pending，不应删除下载器数据。
- 保留已有 qB/TR 多种路径映射、任务标签、同一用户主动移除正确种子的语义。

## File map

| File | Responsibility |
|---|---|
| `crates/downloader/src/lib.rs` | 区分不支持快照和成功空快照；保留 `torrent_matches_snapshot(title, size, name, size)` 行为 |
| `crates/downloader/src/qbit.rs` | qB existing_info/add/remove/completed_files 共用安全 Torrent 身份 |
| `crates/downloader/src/transmission.rs` | 正确处理 HTTP 409 session id；remove 仅唯一安全匹配 |
| `crates/api/src/http/downloaders/task_listing.rs` | 枚举失败/不支持时保留 pending 且上报 neutral `queued` |
| `crates/downloader/tests/qbit.rs` | 相同体积不同名假服务回归 |
| `crates/downloader/tests/transmission.rs` | 标准 409 握手与多种同体积任务误删回归 |
| `crates/api/tests/management/downloaders.rs` | 不支持快照时 >15m pending 不清；真实空快照可清 |

---

### Task 1: qB 同体积不能误判、删错或错 Transfer

**Files:** Modify `crates/downloader/src/qbit.rs:281-296`; Test `crates/downloader/tests/qbit.rs`.

**Interfaces:** `downloader::torrent_matches_snapshot(title: &str, size_bytes: Option<u64>, snapshot_name: &str, snapshot_size: u64) -> bool` 已存在，标题必须匹配，大小已知时也必须匹配；`existing_info` 是 add/remove/completed_files 的唯一选取接缝。

- [ ] **Step 1 — 红测：** 扩展 `qbit.rs` 测试 fake 的 `/api/v2/torrents/info` 返回单条 `TorrentInfo`，hash 为 `unrelated`、name 为 `Other.Movie.2024`、size 为 `1000`、progress 为 `1.0`，其余 JSON 必需字段沿用现有 fake。查询目标是现有 `torrent()` fixture，改 title 为 `The.Matrix.1999`、size_bytes 为 `Some(1000)`；断言 `add_with_options` 确实调用 `/api/v2/torrents/add`，`completed_files` 为 `[]`，`remove(&target_torrent, true)` 不发 `/api/v2/torrents/delete`。再添加名称匹配但不同大小也不选中、名称与大小均匹配仍能跳过重复下载及收集文件的对照断言。
- [ ] **Step 2 — 验红：** `cargo test -p downloader --test qbit same_size_unrelated`，当前匹配 `|| size_matches` 应使测试失败。
- [ ] **Step 3 — 实现：** 在 `existing_info` 中用 `crate::torrent_matches_snapshot(&torrent.title, torrent.size_bytes, &info.name, info.size)` 代替当前“名称匹配或体积匹配”的条件；保留现有 `existing_hash` / `completed_files` 调用关系，避免三条路径规则分叉。可将测试 fake 的 info 响应注入字段放在 `Capture`，保证三个操作读到同一 fake 任务。
- [ ] **Step 4 — 测绿：** `cargo test -p downloader --test qbit && cargo test -p downloader`；确保 `add_is_skipped_when_qbittorrent_already_has_the_torrent` 等现有用例不退化。
- [ ] **Step 5 — Commit：** `git add crates/downloader/src/qbit.rs crates/downloader/tests/qbit.rs && git commit -m "fix(downloader): require title identity for qbit torrent matches"`

### Task 2: Transmission remove 只接受唯一身份

**Files:** Modify `crates/downloader/src/transmission.rs:139-168`; Test `crates/downloader/tests/transmission.rs`.

**Interfaces:** `remove(&self, torrent: &Torrent, delete_files: bool) -> Result<(), DownloaderError>` 保持。未匹配返回 `Ok(())`，**多个**候选身份无法消歧返回 `Err(DownloaderError::Message("ambiguous Transmission torrent identity".into()))`，且不调用 `torrent-remove`。TR `completed_files` 已有更严格的 `exact_name || (exact_size && name_contains_core)`，remove 不得比它更宽。

- [ ] **Step 1 — 红测：** fake `torrent-get` 同时返回两个 `{id:1,name:"The.Matrix.1999",sizeWhenDone:1000}` 与 `{id:2,name:"Other.Movie.2024",sizeWhenDone:1000}`；`remove(Matrix,true)` 的 `torrent-remove` 请求必须仅有 `ids:[1]` 且 `delete-local-data:true`；`remove(Other,true)` 仅 `[2]`。两个相同归一化名称+体积的任务请求应报 ambiguity 且**不**请求 remove；无匹配则 `Ok(())` 不请求 remove。
- [ ] **Step 2 — 验红：** `cargo test -p downloader --test transmission same_size_unrelated`；当前 `|| size_matches` 会把两个 ID 一起删除。
- [ ] **Step 3 — 实现：** 先用 `crate::torrent_matches_snapshot(&torrent.title, torrent.size_bytes, name, size)` 找唯一候选；没有返回 `Ok(())`，超过 1 返回显式歧义错误，恰好 1 则 RPC `torrent-remove` 且 `ids` 为该一个 ID。不要把 `delete_files` 默认为 true；此任务不修改 `completed_files` 的安全规则。
- [ ] **Step 4 — 测绿：** `cargo test -p downloader --test transmission && cargo test -p downloader`。
- [ ] **Step 5 — Commit：** `git add crates/downloader/src/transmission.rs crates/downloader/tests/transmission.rs && git commit -m "fix(downloader): remove only uniquely matched Transmission torrents"`

### Task 3: 标准 Transmission 409 session 握手

**Files:** Modify `crates/downloader/src/transmission.rs:60-110`; Test `crates/downloader/tests/transmission.rs`.

**Interfaces:** `send(&self, payload: &str) -> Result<ureq::http::Response<ureq::Body>, DownloaderError>` 保持。`ureq::Error::StatusCode(409)` 需要从**该错误携带的 HTTP Response** 获取 `X-Transmission-Session-Id`；编码前先验证本仓库 ureq 3 的匹配 API，不能再无 token probe。

- [ ] **Step 1 — 红测：** fake RPC：首次无头返回 `409 Conflict` + `X-Transmission-Session-Id: fresh-id`；带 `fresh-id` 才返回 200 `{"result":"success","arguments":{}}`。`TransmissionDownloader::connect` 必须成功并只发一次 409+一次带头重试；第二次 RPC 使用缓存 token。再模拟服务更换为 `rotated-id`，下一次 409 后重试应更新 token。缺失 header 返回明确 `DownloaderError`，不无限重试。
- [ ] **Step 2 — 验红：** `cargo test -p downloader --test transmission session_challenge`，当前 probe 再获 409 且丢头而失败。
- [ ] **Step 3 — 实现：** inspect ureq 3 `Error::StatusCode` 的 `response()`（或该版本实际 accessor），从第一次 409 Response.headers 读取 header 并 `self.session.lock()` 更新，`make_req(Some(new_token)).send(payload)` **仅重试一次**；拒绝无 header/第二次仍 409 时返回原错误，不把 409 的 body 误解成成功。`rpc` 对正常 200 的 header 更新保持。
- [ ] **Step 4 — 测绿：** `cargo test -p downloader --test transmission && cargo test -p downloader`。
- [ ] **Step 5 — Commit：** `git add crates/downloader/src/transmission.rs crates/downloader/tests/transmission.rs && git commit -m "fix(downloader): use first Transmission 409 session header"`

### Task 4: “不支持快照”不得导致 pending 自动清理

**Files:** Modify `crates/downloader/src/lib.rs:102-110`, `crates/api/src/http/downloaders/task_listing.rs:36-98`; Test `crates/api/tests/management/downloaders.rs`.

**Interfaces:** `Downloader::task_snapshots() -> Result<Vec<TaskSnapshot>, DownloaderError>` 不改签名；默认返回 `Err(DownloaderError::Message("task snapshots unsupported".into()))`，qB/Memory 的实现继续返回可枚举数据；Transmission 在尚无真实枚举实现时返回默认 Err。任务列表将该 id 标成 unknown/not-reachable：只在**真实成功返回快照**时清理 >15min 缺失 pending。

- [ ] **Step 1 — 红测：** 集成测试构造持久化 一条 `DownloaderRow`（kind=`transmission`、url 指向本机 fake RPC，其他字段按现有 `post_dl` fixture），再给该 id 登记 `PendingDownload`（submitted_at 为 `now_secs()-16*60`，Torrent 与已有 `seed_pending_at` 相同）。通过本机 fake RPC 使 `TransmissionDownloader::connect` 成功，但快照调用返回不支持。执行两次 GET `/api/v1/downloaders/tasks`，断言 pending 仍在、任务 state 中性 `queued`。对照：MemoryDownloader 成功空快照，仍清理超过宽限期的 pending。若现有 `client_for_id` 硬编码创建 TR，可在 `task_listing.rs` 同模块 `#[cfg(test)]` 注入 `Arc<dyn Downloader>`，不要用 live 服务。
- [ ] **Step 2 — 验红：** `cargo test -p api --test management transmission_pending_not_pruned`。当前默认 `Ok([])` 被当作 reachable 并删除。
- [ ] **Step 3 — 实现：** `Downloader` 默认改 `Err`，`load_task_snapshots` 继续只在 `Ok(snapshots)` 插入 reachable（现有逻辑），`Err` 记 `tracing::warn!` 并保留 pending；避免把 `spawn_blocking` join 错误也伪装为成功空快照。文档注释明确“不支持 != 确认空”；不要用无 tag 的任务去误删他人 pending。
- [ ] **Step 4 — 测绿：** `cargo test -p downloader && cargo test -p api --test management downloaders && cargo test -p api`。
- [ ] **Step 5 — Commit：** `git add crates/downloader/src/lib.rs crates/api/src/http/downloaders/task_listing.rs crates/api/tests/management/downloaders.rs && git commit -m "fix(tasks): preserve pending when downloader cannot enumerate"`

**整体验收：** `cargo test -p downloader && cargo test -p api && cargo test --workspace`；再手工确认 Transmission 下载器未实现快照时任务列表为中性状态、不自动删 pending。此任务不替 TR 实现进度展示，那需要单独加入真实 `torrent-get` 快照及明确身份标签。
