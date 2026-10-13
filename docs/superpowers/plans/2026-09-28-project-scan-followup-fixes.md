# 全项目扫描后续缺陷修复 Implementation Plan

> **历史状态（2026-10-13 整理）：** 九项扫描整改历史清单；A/B/C 原编号保留，尚未逐 ID 与当前代码/验证证据对账（UNRECONCILED），不是全部未实现或全部关闭。
>
> 本文保留原设计、发现/任务 ID 与当时证据；日期本身不表示过时。旧执行/提交/清理命令仅作历史记录，不应直接重跑；复选框不是当前实现清单。当前依据：[数据库/API review](<../../audits/2026-10-13-database-api-review.md>) 与 [剩余计划](<2026-10-13-database-api-followup.md>)，未覆盖的旧 ID 仍须单独对账。

> **规模规则更新：** 文中的 60 行硬限/整改目标是历史要求；当前 [AGENTS.md](<../../../AGENTS.md>) 为生产函数软限 60、测试软限 80、两者硬限 120。历史测试/行数记录不改写，规模合规不等于功能安全。

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 修复本次全项目扫描确认的九项行为问题，补齐真正执行 HTTP/API 与 React 组件的回归测试，而非再次用测试数量或源码正则宣称全部修复。

**Architecture:** 分为三个可独立交付的子项目：A 下载器身份与 pending 生命周期；B 管理员凭据约束与登出；C 完整榜单分页与刷新协调。文件删除和完成文件选择采用同一个保守身份规则；配置 token 与登录密码的约束在 Store 事务和生产启动同时生效；完整榜单请求以服务端分页信息为准，并由请求代次隔离刷新与追加响应。

**Tech Stack:** Rust workspace（Rust 2024、rusqlite、Axum、ureq）、React 19、TypeScript、Vite、Node `node:test`；组件行为测试新增 Vitest/jsdom 与 Testing Library（只作为开发依赖）。

## Global Constraints

- 基点：`8e01dcb`。此文档覆盖本次报告九项问题；不把此前已撤回的筛选条件切换报告纳入范围。
- `.rs` 文件硬限 800 行；函数/方法硬限 60 行，测试逻辑不豁免；按职责拆 sibling 模块，不通过压缩排版规避硬限。
- 自有 API 保持 `/api/v1` 与 `{ok,data}`/错误信封；Jellyfin 保持官方协议路径。静态图片读取端点仍公开。
- `domain` 只放类型，不进行 IO；SQL 留在 `store`。其它 crate 不依赖 `api`。
- 网络测试只用本机 fake HTTP、fake catalog、注入 Downloader 或 mock fetch；无真实 TMDB/下载器，无 Chromium。
- 每项执行红测→验证失败原因→最小实现→绿测→独立提交。测试命令无匹配用例或仅编译失败不能代替有效红测。
- 生产日志不包含密码、token、PHC 或请求 Authorization；拒绝身份歧义、凭据冲突均记录结构化错误。
- 本轮不实现新的 Transmission 快照功能，不改变缺快照时保留 pending 的行为，不扩展播放器或主题。

## 问题与任务对应

| 报告项 | 缺陷 | 任务 |
|---|---|---|
| 1 | 不同集号被宽松名称匹配认为相同 | A1 |
| 2 | 迁移新密码可继续使用旧 CLI 密钥 | B1 |
| 3 | 豆瓣完整榜单只展示前 20 条 | C1 |
| 4 | 刷新第一页与旧分页响应竞态 | C2 |
| 5 | Transmission 同名但体积冲突仍收集文件 | A2 |
| 6 | 重投同 enclosure 保留旧 submitted_at | A3 |
| 7 | 管理员 PATCH 密码可等于当前 CLI token | B2 |
| 8 | 配置 token 碰撞把成员会话变为管理员 | B3 |
| 9 | 无效 Bearer 遮蔽有效 cookie 的登出撤销 | B4 |
| 测试缺口 | 源码 regex 和复制条件未执行真实行为 | C0、每项回归测试 |

## 文件职责映射

- 新建 `crates/downloader/src/identity.rs`：安全任务匹配及唯一候选判定；`lib.rs` 保持公开函数兼容并 re-export。
- 修改 `crates/downloader/{Cargo.toml,src/lib.rs,src/qbit.rs,src/transmission.rs}`：共用季集/范围约束；允许使用已有 workspace `release` 解析器，不复制不一致的季集正则。
- 新建 `crates/downloader/tests/identity.rs`；修改现有 qB/TR fake 测试：验证实际添加、删除及完成文件结果。
- 修改 `crates/store/src/subscribes.rs` 与 `crates/api/src/http/downloaders/submission.rs`：区分一般 pending 合并与明确重投事件；新建 `crates/store/tests/pending_resubmission.rs`。
- 修改 `crates/store/src/users.rs`、`crates/store/src/lib.rs`、`crates/api/src/bootstrap_credentials.rs`、`crates/api/src/http/{users,auth}.rs`：凭据约束、事务错误与登出候选。
- 新建 `crates/store/tests/cli_credential_constraints.rs`；修改 `crates/api/tests/{bootstrap_credentials.rs,management/authz_sessions.rs,management/user_lifecycle.rs}`：覆盖真实 HTTP 结果。
- 修改 `web/components/collection-grid-view.tsx`、`web/lib/api/discover.ts`：服务端分页、请求代次及刷新状态。
- 新建 `web/vitest.config.ts`、`web/test/behavior/collection-grid-view.test.tsx`、`web/test/behavior/discover-api.test.ts`；修改 `web/package.json` 与 `web/pnpm-lock.yaml`：行为测试可执行门禁，不替换现有 Node suite。

---

## 子项目 A：下载器安全与 pending 生命周期

### Task A1：集号冲突不得通过任务身份匹配（最高优先级）

**Files:** Create `crates/downloader/src/identity.rs`, `crates/downloader/tests/identity.rs`; Modify `crates/downloader/Cargo.toml`, `crates/downloader/src/{lib,qbit,transmission}.rs`; Test `crates/downloader/tests/{qbit,transmission}.rs`。

**Interfaces:** 保持 `torrent_matches_snapshot(title: &str, size_bytes: Option<u64>, snapshot_name: &str, snapshot_size: u64) -> bool`。新私有 `release_units_agree(left: &domain::Release, right: &domain::Release) -> bool`：已解析季号、集号和集范围有冲突即 false；一边有集范围而另一边缺失时，不能仅因宽松分词相似作破坏性匹配。精确归一化标题可通过；非精确标题必须有可确认的相同季集范围、年份无冲突，再用既有名称别名匹配。正片与 Sample 不可互相匹配。

- [ ] **Step 1 — 红测：** 在公开 matcher 上加入下面断言，并在 qB/TR fake 中只返回 E02，用目标 E01 验证不删除、不返回完成文件；qB 不得跳过添加。

```rust
assert!(!downloader::torrent_matches_snapshot(
    "Show.S01.E01.1080p", Some(1000), "Show.S01.E02.1080p", 1000,
));
assert!(!downloader::torrent_matches_snapshot(
    "Show.S01E01-E04", Some(1000), "Show.S01E01-E05", 1000,
));
assert!(downloader::torrent_matches_snapshot(
    "Show.S01E01.1080p", Some(1000), "Show S01 E01 1080p", 1000,
));
```

- [ ] **Step 2 — 验红：** `cargo test -p downloader --test identity`；当前独立 E01/E02 被 `is_ignored_token` 丢弃，应产生错误的 true。若 `release::parse` 不支持此拼写，先在 `crates/release/tests/parse.rs` 增加解析回归，禁止用“无法解析”作为允许匹配的理由。
- [ ] **Step 3 — 实现：** 新模块先比较季集/范围，再进行标题别名和可选精确体积判断；新增 `release.workspace = true`（先确认 workspace 已有此依赖声明）。`names_match` 可保留非破坏性用途，但 qB/TR 任务操作只能调用安全 matcher。qB `existing_info` 收到多个候选时返回歧义错误而非 `.find()` 任取第一条，测试同样确保没有 delete 请求。
- [ ] **Step 4 — 绿测：** `cargo test -p release && cargo test -p downloader`。测试同剧不同季、不同集、不同范围、Sample、未知体积，以及中英文别名前缀但相同季集的正例，保持实际 PT 标题支持。
- [ ] **Step 5 — Commit：** `git add crates/downloader crates/release && git commit -m "fix(downloader): reject conflicting episode identities"`；只暂存本任务修改，不捎带无关 crate 内容。

### Task A2：Transmission 完成文件选择共用安全、唯一身份

**Files:** Modify `crates/downloader/src/transmission.rs:195-219`; Test `crates/downloader/tests/transmission.rs`。

**Interfaces:** `completed_files(&self, torrent: &Torrent) -> Result<Vec<PathBuf>, DownloaderError>` 保持。0 个身份候选返回空；多个候选返回歧义错误；唯一候选且完成才返回文件。身份选择在 progress 过滤之前消歧，避免因为其中一个未完成而误认另一个任务。

- [ ] **Step 1 — 红测：** fake 返回标题相同、体积 2000、已完成任务，目标已知体积 1000；结果必须为空。返回两个标题和大小均相同的任务时，结果必须 Err，不能聚合两组文件。

```rust
let mut target = torrent(); // 本测试文件现有完整 fixture
 target.size_bytes = Some(1000);
// fixture 的 torrent-get 返回同名 sizeWhenDone=2000、percentDone=1.0。
assert!(dl.completed_files(&target).unwrap().is_empty());
```

- [ ] **Step 2 — 验红：** 为以上场景命名 `completed_files_rejects_known_size_conflict` 与 `completed_files_rejects_ambiguous_identity`；运行 `cargo test -p downloader --test transmission completed_files_`。当前 exact_name 分支绕过大小验证，必失败。
- [ ] **Step 3 — 实现：** 用 A1 的公开 matcher 过滤 torrent-get 身份候选；禁止 `exact_name || (exact_size && name_contains_core)` 的独立口径；0/1/多候选处理与 remove 一致，保留路径映射和完成状态检测。移除不再使用的 `name_contains_core`，避免出现第二套安全规则。
- [ ] **Step 4 — 绿测：** `cargo test -p downloader && cargo test -p subscribe`，验证正常完成文件映射及目标未知大小正例。
- [ ] **Step 5 — Commit：** `git add crates/downloader/src/transmission.rs crates/downloader/tests/transmission.rs && git commit -m "fix(downloader): collect only uniquely identified Transmission files"`。

### Task A3：明确重投更新时间，普通合并不重置宽限期

**Files:** Modify `crates/store/src/subscribes.rs:90-138`, `crates/api/src/http/downloaders/submission.rs:90-108`; Create `crates/store/tests/pending_resubmission.rs`; Test `crates/api/tests/management/downloaders.rs`。

**Interfaces:** 保留 `merge_pending` 的普通合并语义，新增 `Store::record_pending_submission(subscribe_id: domain::SubscribeId, score: i32, pending: &store::PendingDownload) -> Result<(), StoreError>`：只在**确认本次下载器提交成功**后调用，冲突时更新 `submitted_at`、状态及目标下载器。搜索路径若也发生真实重投必须使用该接口；仅重复持久化候选不能延后宽限期。

- [ ] **Step 1 — 红测：** 先写入旧 pending 时间 1000，标为 imported 或保留 active，明确重新提交时间 3000；回读必须为 3000。对照：普通 `merge_pending` 合并同 enclosure 不改变原时间。

```rust
store.record_pending_submission(subscribe_id, 80, &fresh_pending).unwrap();
let rows = store.load_pending(subscribe_id).unwrap();
assert_eq!(rows[0].1.submitted_at, Some(3000));
```

测试 fixture 复用完整 `Subscribe`、`PendingDownload` 构造；两次记录同 enclosure，用不同 `submitted_at` 验证，不断言 SQL 文本。

- [ ] **Step 2 — 验红：** `cargo test -p store --test pending_resubmission`，现有冲突 UPDATE 无 submitted_at。
- [ ] **Step 3 — 实现：** 抽取私有 upsert helper，由普通合并明确传 `refresh_submitted_at=false`，真实提交传 true；SQL 在 true 路径写 `submitted_at=excluded.submitted_at`。manual submit 的 add_result=Ok 分支调用新接口，失败的添加不得延长计时。
- [ ] **Step 4 — 绿测：** `cargo test -p store && cargo test -p api --test management downloaders && cargo test -p api --test management delivery`。API 测试旧 pending 超 15 分钟，重新 POST 成功后立即 GET tasks，在成功空快照下仍应保留 fresh pending。
- [ ] **Step 5 — Commit：** `git add crates/store crates/api/src/http/downloaders/submission.rs crates/api/tests/management/downloaders.rs && git commit -m "fix(tasks): refresh grace period only on actual torrent resubmission"`。

---

## 子项目 B：凭据隔离与真实会话撤销

### Task B1：旧 CLI token 不得再次作为迁移目标密码

**Files:** Modify `crates/api/src/bootstrap_credentials.rs`, `crates/store/src/users.rs`; Test `crates/api/tests/bootstrap_credentials.rs`, `crates/store/tests/cli_credential_constraints.rs`。

**Interfaces:** 新增只读 `Store::admin_password_matches_known_bearer(admin_id: UserId, proposed_password: &str, current_cli: &str) -> Result<bool, StoreError>`：比较 proposed_password 与 current_cli、marker、固定管理员历史 bearer 字符串；不是用 proposed_password 去验证旧 PHC。仅对迁移/恢复路径约束历史 bearer，不能把 Keep 环境里的未使用 ADMIN_PASSWORD 当作覆盖现存密码的命令。

- [ ] **Step 1 — 红测：** 创建旧 admin 密码/marker/token 都为 A，传 token B、ADMIN_PASSWORD=A；应拒绝且密码、marker、token 不变。传独立 C 则 Rotate，执行后 A 密码和 Bearer 均无效、B Bearer 与 C 登录可用。

```rust
let result = prepare_admin_credentials(
    &store, "new-cli-B", Some("old-cli-A"), BootstrapMode::Production,
);
assert!(matches!(result, Err(BootstrapCredentialsError::PasswordEqualsKnownBearer)));
```

- [ ] **Step 2 — 验红：** `cargo test -p api --test bootstrap_credentials replacement_password_cannot_reuse_old_cli`；当前返回 Rotate(A,B)。
- [ ] **Step 3 — 实现：** 在已有 fresh/legacy 口令选择后、返回可写 action 前做独立口令约束；新增 `PasswordEqualsKnownBearer` 错误，信息不打印 A/B/C。读取候选 SQL 用 `collect::<Result<Vec<String>, _>>()?`，不通过 `filter_map(Result::ok)` 忽略数据库错误。身份使用固定 id，不硬编码 login=`admin` 误查重命名账号。
- [ ] **Step 4 — 绿测：** `cargo test -p store && cargo test -p api --test bootstrap_credentials`。包含旧库无 marker、PHC 旧密码及仅 CLI 轮换不覆盖现存独立密码的正例。
- [ ] **Step 5 — Commit：** `git add crates/store/src/users.rs crates/api/src/bootstrap_credentials.rs crates/api/tests/bootstrap_credentials.rs crates/store/tests/cli_credential_constraints.rs && git commit -m "fix(auth): reject historical bearer reuse as migration password"`。

### Task B2：管理员改密不能等于当前 CLI bearer

**Files:** Modify `crates/store/src/{users,lib}.rs`, `crates/api/src/http/users.rs`; Test `crates/api/tests/management/user_lifecycle.rs`, `crates/store/tests/cli_credential_constraints.rs`。

**Interfaces:** 新增 typed `StoreError::CredentialConflict`。在 `update_user_and_password` 的同一事务读取 marker 与 token 归属，若目标为系统管理员且 password 等于当前 CLI marker，拒绝更新和撤销会话；HTTP 映射 400 `user.credential_conflict`，不得返回 500。成员可使用与其它成员相同的密码，不能全局禁止常规密码重用。

- [ ] **Step 1 — 红测：** 以合法 admin 会话或 CLI PATCH seed admin `password=current-cli`，必须 400，原密码仍可登录，原会话未被撤销。PATCH 新独立 P2 返回成功，普通会话失效、CLI 保留。

```rust
assert_eq!(patch_same_as_cli.status(), StatusCode::BAD_REQUEST);
assert!(store.password_matches_without_session("admin", "original-password").unwrap());
```

- [ ] **Step 2 — 验红：** `cargo test -p api --test management admin_password_cannot_equal_cli`；当前 PATCH 成功。
- [ ] **Step 3 — 实现：** 借用 B1 的只读候选校验，但事务内至少核当前 marker，保证与同时配置变更无 TOCTOU；出现冲突先返回 typed error，不先 UPDATE 再留下修改。HTTP 错误只说明必须与运维凭据不同。
- [ ] **Step 4 — 绿测：** `cargo test -p store && cargo test -p api --test management user_lifecycle && cargo test -p api --test jellyfin`。通过两套登录路径验证被拒的 CLI 值没有成为有效登录密码。
- [ ] **Step 5 — Commit：** `git add crates/store/src/{users,lib}.rs crates/api/src/http/users.rs crates/api/tests/management/user_lifecycle.rs crates/store/tests/cli_credential_constraints.rs && git commit -m "fix(auth): preserve password and CLI credential separation on updates"`。

### Task B3：CLI token 碰撞禁止转移会话所有权

**Files:** Modify `crates/store/src/users.rs:525-557`, `crates/store/src/lib.rs`; Test `crates/store/tests/cli_credential_constraints.rs`。

**Interfaces:** `apply_cli_token_tx` 开始先查询待登记 token 的既有 user_id。属于其它用户时返回 `StoreError::CredentialConflict`，整个事务回滚；属于相同 admin 时允许续期。写入用受限 UPSERT 而非 REPLACE：

```sql
INSERT INTO user_tokens(token,user_id,created_at) VALUES (?1,?2,?3)
ON CONFLICT(token) DO UPDATE SET created_at=excluded.created_at
WHERE user_tokens.user_id=excluded.user_id
```

确认 affected rows=1，否则冲突；先撤旧后发现冲突也须通过事务回滚恢复旧 token。

- [ ] **Step 1 — 红测：** 成员持有 X，管理员持有 A；登记管理员 CLI=X 必须 Err，`user_by_token(X)` 仍为成员，A 仍为管理员，marker 仍 A；Seed/Rotate/Keep 三条写入路径均复用同一约束。

```rust
assert!(store.register_current_cli_token(admin_id, "member-X").is_err());
assert_eq!(store.user_by_token("member-X").unwrap().unwrap().id, member_id);
assert_eq!(store.user_by_token("admin-A").unwrap().unwrap().id, admin_id);
```

- [ ] **Step 2 — 验红：** `cargo test -p store --test cli_credential_constraints cli_collision`；当前成员 X 变为管理员。
- [ ] **Step 3 — 实现：** token 查归属、撤旧、受限 UPSERT、marker 更新都在现有 tx 内；SQL/冲突日志只打 user_id，不打 token。同属管理员续期路径不误杀有效普通会话。
- [ ] **Step 4 — 绿测：** `cargo test -p store && cargo test -p api --test bootstrap_credentials`，补触发器写入故障回滚和同属管理员合法续期。
- [ ] **Step 5 — Commit：** `git add crates/store/src/{users,lib}.rs crates/store/tests/cli_credential_constraints.rs && git commit -m "fix(auth): fail closed on CLI and member token collision"`。

### Task B4：登出明确撤销 cookie 和 Bearer 会话

**Files:** Modify `crates/api/src/http/auth.rs:66-104`; Test `crates/api/tests/management/authz_sessions.rs`。

**Interfaces:** 保持 POST `/api/v1/auth/logout` 公开且幂等。定义请求携带的普通 Bearer 和 `mc_session` cookie 都属于本次 logout 候选；去重后都撤销，保留 Store 对当前系统 CLI 的保护。不要只选择 Header 而丢掉 cookie。撤销数据库出错时返回结构化 500，不伪装“已撤销”；成功才返回 200 并清 cookie。

- [ ] **Step 1 — 红测：** 登录签发 S，发送 `Authorization: Bearer stale-invalid` + `Cookie: mc_session=S` logout；随后分别用 S Bearer/cookie 请求 auth/me 均 401。两个不同有效普通会话一起携带时都撤销；同 token 只需撤一次；CLI+普通 cookie 时 CLI 保留、cookie 会话失效。

```rust
assert_eq!(logout_response.status(), StatusCode::OK);
assert_eq!(me_with_old_cookie_token.status(), StatusCode::UNAUTHORIZED);
```

- [ ] **Step 2 — 验红：** `cargo test -p api --test management logout_invalid_bearer_valid_cookie`；当前 cookie S 仍有效。
- [ ] **Step 3 — 实现：** 认证中间件 `request_token` 的优先级不必改；logout 单独收集候选，使用同一 store lock 和一个撤销事务接口 `Store::delete_session_tokens(tokens: &[String]) -> Result<usize, StoreError>`，过滤受保护 CLI token；异常不能用 `let _` 静默吞掉。保留 cookie 的 Path/HttpOnly/SameSite 与清理属性。
- [ ] **Step 4 — 绿测：** `cargo test -p store && cargo test -p api --test management authz_sessions`，包含 revoke 触发器失败与成员隔离。
- [ ] **Step 5 — Commit：** `git add crates/store/src/users.rs crates/api/src/http/auth.rs crates/api/tests/management/authz_sessions.rs && git commit -m "fix(auth): revoke all presented logout sessions safely"`。

---

## 子项目 C：完整榜单分页与刷新协调

### Task C0：先建立可执行组件/API 测试接缝

**Files:** Modify `web/package.json`, `web/pnpm-lock.yaml`; Create `web/vitest.config.ts`, `web/test/behavior/{collection-grid-view.test.tsx,discover-api.test.ts}`。

**Interfaces:** 新增脚本 `test:behavior: vitest run --config vitest.config.ts`，include 仅 `test/behavior/**/*.test.{ts,tsx}`；保持 `pnpm test` 的 Node suite 不变。配置 jsdom、根 alias `@ -> web/`，mock scroll restoration、PosterCard 和 PageNav 的展示层，不 mock被测请求/状态流程。DOM IntersectionObserver 用可主动触发的 fake，网络用 mock fetch 或 deferred promise，不用 sleep。

- [ ] **Step 1 — 接缝：** 安装开发依赖 `pnpm add -D vitest jsdom @testing-library/react`，选择与现有 Vite 7/React 19/本机 Node 兼容的版本并提交 lockfile。组件 mount/unmount 正例先验证不依赖真实服务器。
- [ ] **Step 2 — 真行为红测：** 新测试渲染实际 `CollectionGridView`，让服务端第一页返回 20 条、总数 73、has_more=true；触发哨兵再返回第二页，DOM 必须出现第二页影片。API 测试直接调用 `fetchDiscoveryPage` 后调用 `browseDiscoveryCollection(ref,20,{},1,"full")`，mock fetch 记录完整片单 endpoint 被实际请求且 totalResults=73。

```tsx
const page1 = deferred();
const page2 = deferred();
// deferred<T> 在测试 helper 定义 resolve/reject/promise，手动按次序解锁。
render(<CollectionGridView collectionRef="douban:movie:top-rated" />);
await act(async () => page1.resolve(firstCollection));
triggerIntersection();
await act(async () => page2.resolve(secondCollection));
expect(screen.getByText("Second-page movie")).toBeTruthy();
```

`firstCollection`/`secondCollection` 使用完整 `DiscoveredCollectionData` fixture，尤其 page/totalResults/hasMore 字段，不省略类型必需字段；mock fetch API fixture 使用完整信封及 DTO。

- [ ] **Step 3 — 验红：** `cd web && pnpm run test:behavior`，豆瓣第二页用例应失败，而测试加载/配置本身成功。
- [ ] **Step 4 — 隔离门禁：** 把手工复制 reloadKey 的旧测试及 regex 检查保留为辅助合同或删除，但不能以它们替代行为验收；Node suite 不重复发现 Vitest 文件，必要时现有 `test` 改为明确 `node --test test/*.test.mjs`。
- [ ] **Step 5 — Commit：** 先随 C1 的实现一起提交行为接缝，不把失败红测提交为默认绿色成果；文档记录 `test:behavior` 为新增必跑命令。

### Task C1：豆瓣完整榜单按服务端 has_more 续载

**Files:** Modify `web/components/collection-grid-view.tsx`, `web/lib/api/discover.ts`; Test C0 的组件/API 行为测试。

**Interfaces:** `browseDiscoveryCollection` full 模式仍真实请求第一页；TMDB 与豆瓣完整分区都用一致 `page_size=20`，不用未生效的 `DOUBAN_FULL_LIMIT=500` 假设一次取全。`hasMore=collection.hasMore`；loadNextPage 不限制 provider=tmdb。豆瓣未知 totalResults 不能展示伪准确总量（后端当前 count 只是当前页数量），可展示已加载数量并使用 hasMore 控制哨兵。

- [ ] **Step 1 — 红测：** C0 的豆瓣响应 page1 hasMore=true、page2 hasMore=false；触发哨兵后 DOM 包含两页并显示加载终态，request URL 有 `source=douban&page=2`（查询参数顺序不敏感）。普通不支持 full listing 的分区不新增入口。
- [ ] **Step 2 — 验红：** `cd web && pnpm run test:behavior`；当前 provider gate 阻止豆瓣 page2。
- [ ] **Step 3 — 实现：** 移除第一屏 `provider === "tmdb" && collection.hasMore` 与 loadNextPage 的 provider 限制；只使用实际 hasMore。取消 DOUBAN_FULL_LIMIT，两个源均固定与后端原生分页一致的 20，不能使用更大的 page_size 后截断上游页引发漏项。
- [ ] **Step 4 — 绿测：** `cd web && pnpm exec tsc --noEmit && pnpm test && pnpm run test:behavior`；`cargo test -p api --test management discover`，豆瓣第一页空与尾页不足20条正例。
- [ ] **Step 5 — Commit：** `git add web && git commit -m "fix(web): paginate complete Douban collections with behavioral tests"`；只暂存 C0/C1 文件。

### Task C2：刷新和追加使用互斥代次，旧响应不得污染新列表

**Files:** Modify `web/components/collection-grid-view.tsx`; Test `web/test/behavior/collection-grid-view.test.tsx`。

**Interfaces:** 新增 `firstPageLoading` 状态和 `firstPageLoadingRef`、`requestGenerationRef`；每次路由切换/刷新递增代次、abort 已有 pageController，首屏请求 active 时禁止 loadNextPage 和 IntersectionObserver。任何首屏或追加响应写状态前均核 capturedGeneration=currentGeneration 且 ref 一致、signal 未取消。刷新保留旧 DOM，但旧 hasMore/nextPage 不可触发新请求；仅新代次成功的第一页替换 DOM 与 cursor。

- [ ] **Step 1 — 红测：** mount 已加载三页快照 nextPage=4；用 deferred 首屏触发 touch 下拉刷新；刷新 pending 期间触发哨兵，不应请求 page4。刷新完成替换为新 page1，然后追加必须请求 page2。再使旧 page4 的 promise 在新 page1 后完成（mock 不理 abort），DOM/cursor 不可被旧响应修改。

```tsx
fireEvent.touchStart(scrollRoot, { touches: [{ clientY: 0 }] });
fireEvent.touchMove(scrollRoot, { touches: [{ clientY: 100 }] });
fireEvent.touchEnd(scrollRoot);
triggerIntersection();
expect(requestedPages).not.toContain(4);
```

- [ ] **Step 2 — 验红：** `cd web && pnpm run test:behavior`；当前刷新期间哨兵可调用旧 nextPage。
- [ ] **Step 3 — 实现：** 抽取 `refreshFirstPage`，手势和 page1 retry 共用，刷新同步设锁→删除当前 snapshot→递增代次→abort pagination→重新请求 full page1。finally 仅当前代次可解锁，snapshot 在刷新期间不写旧 items。页面换 ref 与同 ref 刷新分别处理，禁止 reloadKey>0 导致切换新片单仍沿用旧列表。
- [ ] **Step 4 — 绿测：** `cd web && pnpm exec tsc --noEmit && pnpm test && pnpm run test:behavior`。行为测试覆盖失败保旧 DOM、点击“重试刷新”实际请求 page1、下一页失败只重试该页、另一个片单快照不被删除、StrictMode cleanup、切换 ref 后迟到响应隔离。
- [ ] **Step 5 — Commit：** `git add web/components/collection-grid-view.tsx web/test/behavior/collection-grid-view.test.tsx && git commit -m "fix(web): serialize collection refresh and pagination generations"`。

---

## 执行顺序与发布验收

建议顺序 A1 → A2 → B1 → B2 → B3 → B4 → A3 → C0/C1 → C2。每个子项目可以独立验收；A1 是 A2 的接口依赖，B1 的 typed credential 约束在 B2/B3 复用，C0 是 C1/C2 的测试前提。不要同时改共享 users.rs 或 collection-grid-view.tsx。

- [ ] 后端：每个修改 crate 先 `cargo test -p <crate>`；最终 `cargo test --workspace`，收集后台任务最终 exit code，不因中途 partial passed 声称全绿。
- [ ] 前端：`cd web && pnpm exec tsc --noEmit && pnpm test && pnpm run test:behavior && pnpm build`；仅源码正则/纯 helper PASS 不算行为验收。
- [ ] 数据安全：不同季集/范围任务不可下发删除；TR 已知体积冲突不可收集；歧义多候选不可任取；失败后 pending 和源文件不因本轮修复而清空。
- [ ] 凭据安全：迁移 A→B 不允许 A 作为新密码；PATCH password=CLI 被拒且会话不变；成员 X 与 CLI 碰撞不提升权限；logout stale Bearer+valid cookie 真正撤销 cookie session；系统 CLI 保留。
- [ ] 用户行为：豆瓣完整榜单能加载第二页；刷新 pending 时不会请求旧页；首屏失败按钮请求 page1；刷新失败仍看得到旧列表。
- [ ] `git diff --check` 与实际函数规模检查；记录 HEAD、各 test 命令结果及尚未验证项。交付不得宣称“全项目已无 bug”，只声明本计划场景通过。

## 本轮不纳入的追加观察

本次扫描还看到 Move 在 scrape/hook/ledger 持久化之前移除下载源的风险，但上一条九项确认清单没有将其纳入。另立后续任务：需要文件系统与 SQLite 的可恢复转移协议、失败回滚/重试行为，不能用此次身份匹配补丁顺带做跨边界事务重构。
