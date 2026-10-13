# 用户口令与会话安全收口 Implementation Plan

> **历史状态（2026-10-13 整理）：** 口令迁移和会话安全历史方案；后续 bootstrap/CLI 生命周期方案调整了初始化与撤销决策，须结合当前实现核对，不照搬旧迁移步骤。
>
> 本文保留原设计、发现/任务 ID 与当时证据；日期本身不表示过时。旧执行/提交/清理命令仅作历史记录，不应直接重跑；复选框不是当前实现清单。当前依据：[数据库/API review](<../../audits/2026-10-13-database-api-review.md>) 与 [剩余计划](<2026-10-13-database-api-followup.md>)，未覆盖的旧 ID 仍须单独对账。

> **规模规则更新：** 文中的 60 行硬限/整改目标是历史要求；当前 [AGENTS.md](<../../../AGENTS.md>) 为生产函数软限 60、测试软限 80、两者硬限 120。历史测试/行数记录不改写，规模合规不等于功能安全。

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 所有用户输入密码都哈希、存量密码登录升级不静默失败也不覆盖并发改密、初始管理员密码不再作为明文会话 token 存入 SQLite。

**Architecture:** 先在 `store` 固定密码与迁移的公开可观测行为，再把管理面初始化改成独立的初始管理员密码与 CLI token。绝不通过删除全部历史 `user_tokens` 强制踢走已存在的用户；生产启动对旧实例做显式安全前置检查和有界旋转。区分「数据库可读会话 token」与「可恢复的登录密码」：本任务解决后者；token 本身仍为 bearer 凭证，备份依旧敏感。

**Tech Stack:** Rust 2024、argon2 0.5、password-hash 0.5、rusqlite 0.37、axum 0.8；不修改 ADR-0003 的 Site 凭证规则。

## Global Constraints

- `domain` 纯类型；`store` 不依赖 `api`；其它 crate 不依赖 `api`。
- 登录口令遵循 ADR-0010；Jellyfin `AuthenticateByName` 和 `/api/v1/auth/login` 共用 `Store::verify_user_password`，两边都必须验收。
- 口令、会话 token、PHC 哈希均不进入结构化日志；失败只记录用户 id、阶段、错误种类。
- 新建/修改用户输入永远按**明文输入**处理，不允许按 `$argon2` 前缀跳过哈希。已存 PHC 与旧明文只在验证时区分。
- 迁移失败必须 fail closed；用户身份、角色、历史用户密码和已有正常会话不得被意外改写。
- 文件硬限 800 行，函数硬限 60 行；测试逻辑不豁免。

## Product decision（开发前写入 ADR-0010）

1. 配置新增 `CRAWLER_MEDIA_ADMIN_PASSWORD` 与 `_FILE`，仅供**首次创建管理员**及「旧管理员口令等于旧 CLI token」的显式迁移使用；`CRAWLER_MEDIA_TOKEN` 继续是 CLI bearer token，二者必须非空且不同。不要将 ADMIN_PASSWORD 写入 SQLite 的 `user_tokens`。
2. 生产首次启动缺 ADMIN_PASSWORD 或两值相等时在**写入管理员行之前**报清晰启动错误。已有管理员且安全（当前 token 不可用于密码登录）的旧安装，无新配置照旧启动、密码不变。
3. 对已存在管理员，如果旧 CLI token 仍能通过管理员密码校验，**要求**配置不同的 ADMIN_PASSWORD；先安全更新管理员密码，但保留当前配置的 CLI bearer（仅当用户接受继续持有该 bearer 时）；如要吊销它，必须同步更新 CLI secret，不能悄悄踢掉现有运维入口。不可修改已经由管理员改过的独立密码；旧备份与已暴露的 token 不可逆，发布说明提示轮换 CLI secret 和历史备份。判断旧口令时使用**只读且不签发 token**的 Store 接缝，禁止调用 `verify_user_password` 作检查。
4. 新安装默认仍登记 `CRAWLER_MEDIA_TOKEN` 为管理员 CLI bearer，但数据库读者获得 bearer 凭证是当前系统的既有安全边界；不要再宣传「数据库泄露后仍不能冒充管理员」。本计划保证初始**登录密码**不会被同一个 token 反查得到。若产品需要保护 bearer 凭证，即后续单独设计 token 摘要存储并解决 `latest_token_for_user` 无法反查原文的问题。

## File map

| File | Responsibility |
|---|---|
| `docs/adr/0010-password-hashing.md` | 明确管理员密码/CLI token 分离、旧实例迁移及 bearer 边界 |
| `crates/store/src/password.rs` | PHC 解析、salted Argon2id 哈希、旧明文校验 |
| `crates/store/src/users.rs` | `set_password`、原子密码验证/旧口令升级/会话发放；创建用户写入密码的事务方法 |
| `crates/store/tests/password_migration.rs` | 文件库密码、更新失败、CAS 冲突、错误密码/停用用户 |
| `crates/api/src/config.rs` | 新管理员密码环境变量及 `_FILE` 解析 |
| `crates/api/src/main.rs` | 启动前预检；把管理员初始密码独立传入初始化 |
| `crates/api/src/management/state.rs` | 初始化管理员的口令与 CLI token 分离，不重置既有独立密码 |
| `crates/api/src/http/users.rs` | 成员创建与密码设置原子化（不遗留无密码行） |
| `crates/api/tests/management/user_lifecycle.rs` | v1 创建、重设密码、旧实例升级 HTTP 行为 |
| `crates/api/tests/jellyfin.rs` | Jellyfin 管理员密码登录保持正常 |

---

### Task 1: 新输入密码必哈希，旧 PHC 登录仍正常

**Files:** Modify `crates/store/src/users.rs`, `crates/store/src/password.rs`; Create `crates/store/tests/password_migration.rs`.

**Interfaces:** `Store::set_password(UserId, &str) -> Result<(), StoreError>` 不改签名；`Store::verify_user_password(&str,&str) -> Result<Option<String>, StoreError>` 保持。

- [ ] **Step 1 — 红测：** 创建 User（`domain::User`，`UserRole::Member`），调用 `set_password(id, "$argon2id$not-a-valid-phc")`；从 tempdir 的 `app.db` 只读查询 `users.password`，断言值以 `$argon2id$` 开头、**不等于**输入、`verify_user_password(login, 原输入)` 返回 `Some(token)`。再用输入 `"$argon2id$valid-looking-text"` 与普通密码分别测随机 salt（两次设置得到不同 PHC）。
- [ ] **Step 2 — 验红：** `cargo test -p store --test password_migration literal_hash_prefix`：当前会存原文且不能登录。
- [ ] **Step 3 — 最小实现：** `set_password` 对所有调用的 `password` 无条件执行 `password::hash_password(password)?`；不要接受调用方传入的预哈希串。`verify_password` 仍只依据**库中** PHC 前缀判断旧数据。
- [ ] **Step 4 — 测绿：** `cargo test -p store && cargo test -p api --test management user_lifecycle && cargo test -p api --test jellyfin authenticate_by_name`。
- [ ] **Step 5 — Commit：** `git add crates/store/src/{users,password}.rs crates/store/tests/password_migration.rs && git commit -m "fix(auth): always hash user-supplied passwords"`。

### Task 2: 明文升级与签发 token 原子、CAS 且失败可见

**Files:** Modify `crates/store/src/users.rs`, `crates/store/tests/password_migration.rs`.

**Interfaces:** 上述 `verify_user_password` 保持；`StoreError::PasswordHash(String)` 已存在。对旧明文行使用 `WHERE id=? AND password=? AND enabled=1` 的条件 UPDATE，检查恰好影响一行。

- [ ] **Step 1 — 红测：** temp DB 用 SQL 插入旧明文。验证成功登录后 `users.password` 变 PHC，且 `user_tokens` 只增加一条；用 SQLite `BEFORE UPDATE OF password` 触发器 `RAISE(FAIL, 'reject upgrade')` 注入错误，断言 `verify_user_password` 返回 `Err`、密码仍明文、token 数量**未增加**；去掉触发器后重试成功。并发接缝写在 `users.rs` 的 `#[cfg(test)]`（不是测试私有 SQL 文本）：打开两个 `Store` 连接，A 读出旧口令，B 改密，再以 A 的旧值调用生产路径同一私有 `finish_verified_login(user_id:&str, observed_password:&str, new_hash:Option<&str>) -> Result<Option<String>, StoreError>`，断言 CAS 失败时不签 token、B 新密码仍可登录。不得用 sleep 或 live 网络。
- [ ] **Step 2 — 验红：** `cargo test -p store --test password_migration`：当前忽略升级失败并可能发 token。
- [ ] **Step 3 — 最小实现：** `finish_verified_login` 在同一 `app` SQLite 事务中完成旧明文的条件 UPDATE（`WHERE id=? AND password=? AND enabled=1`）和 token INSERT，检查 `changed == 1` 再提交；`hash_password` 错误转 `StoreError::PasswordHash` 并 `tracing::error!`（不写明文）。CAS 失败返回 `Ok(None)`，且不插 token；SQLite 错误向上传播，事务回滚。哈希行验证成功时同样先在事务内用 `SELECT password,enabled` 确认观察值仍匹配、再签发 token；不要把锁或 SQL 事务跨 `await` 持有。`verify_user_password` 仅负责查询、验证并调用同一私有完成函数。
- [ ] **Step 4 — 测绿：** `cargo test -p store && cargo test -p api --test management user_lifecycle && cargo test -p api --test jellyfin authenticate_by_name`。
- [ ] **Step 5 — Commit：** `git add crates/store/src/users.rs crates/store/tests/password_migration.rs && git commit -m "fix(auth): make legacy-password upgrade atomic and fail closed"`。

### Task 3: 初始化管理员秘密与 CLI token 分离（含既有实例）

**Files:** Modify `docs/adr/0010-password-hashing.md`, `crates/store/src/users.rs`, `crates/api/src/config.rs`, `crates/api/src/main.rs`, `crates/api/src/management/state.rs`, `crates/api/tests/management/user_lifecycle.rs`, `crates/api/tests/jellyfin.rs`; add focused `crates/api/tests/bootstrap_credentials.rs`.

**Interfaces:** 保留现有 `ApiState::new/new_arc` 的测试/开发注入入口；新增接收 `admin_password` 的生产初始化入口（明确签名 `new_arc_with_admin_password(store, token: String, admin_password: String, profiles, fetcher, downloader, library_root) -> Result<ApiState, ApiStateError>`）。当前测试构造器继续供 fixtures 使用，但生产 `main` 必须调用显式入口；不要用参数默认值把 token 冒充 password。

- [ ] **Step 1 — 红测：** 新 temp DB，用不同的 `"cli-secret"`、`"admin-pass"` 初始化；`verify_user_password("admin", "admin-pass")` 成功，`verify_user_password("admin", "cli-secret")` 失败；`user_tokens` 有 CLI bearer 但**无 admin-pass**。重新打开并以另一套 env password 初始化，不应覆盖已设置的独立管理员密码。旧 temp DB 模拟 `users.password` 是旧明文 token：缺新 password 时启动应显式失败且 DB 未变化；提供新且不同 password 后启动可用新密码，旧值不再能密码登录，并保留可用 CLI bearer 入口；现有管理员已改成独立密码时无需再次修改。
- [ ] **Step 2 — 验红：** `cargo test -p api --test bootstrap_credentials`（当前生产初始化将 token 同时当密码与 bearer）。
- [ ] **Step 3 — 实现：** `ServerConfig::from_env` 增加 `CRAWLER_MEDIA_ADMIN_PASSWORD` / `_FILE`（复用 `secret`）；将缺少配置的校验放在 `Store::open` 之后但**管理员写入之前**，且生产报错不暴露 secret。定义只读 `Store::password_matches_without_session(login:&str, password:&str)->Result<bool,StoreError>`：查 `users.password` 并调用现有 `password::verify_password`，绝不修改数据库/签发 token。初始化新 admin 的 password 和 CLI token 分别使用不同字符串；现存独立密码不重置。老实例旧密码与 CLI token 一致时，提供不同 ADMIN_PASSWORD 后安全更新用户密码、并清理此前遗留的旧明文密码会话 token；仅在当前 CLI token 已准备好重登记并验证时执行清理，整个 app.db 变更必须同事务或提供可重试恢复步骤，防止重启时失去运维入口。旧版本历史备份中明文仍在，更新 ADR 给出轮换与销毁建议。
- [ ] **Step 4 — 测绿：** `cargo test -p api --test bootstrap_credentials && cargo test -p api --test management user_lifecycle && cargo test -p api --test jellyfin && cargo test -p api --test users`。
- [ ] **Step 5 — Commit：** `git add docs/adr/0010-password-hashing.md crates/store/src/users.rs crates/api/src/{config,main}.rs crates/api/src/management/state.rs crates/api/tests/{bootstrap_credentials,jellyfin.rs} crates/api/tests/management/user_lifecycle.rs && git commit -m "fix(auth): separate bootstrap admin password from CLI bearer"`。

### Task 4: 用户创建与修改密码的写入一致性

**Files:** Modify `crates/store/src/users.rs`, `crates/api/src/http/users.rs`, `crates/api/tests/management/user_lifecycle.rs`.

**Interfaces:** `Store::insert_user_with_password(&User, &str) -> Result<(), StoreError>`：先哈希后在 `app` 单事务插入新用户及密码。`Store::update_user_and_password(&User, password:&str) -> Result<(), StoreError>`：同事务保存用户、哈希密码、撤销该用户 tokens；不传密码时调用方保留 `save_user` 既有行为。哈希之前不要写入用户行。

- [ ] **Step 1 — 红测：** `POST /api/v1/users` 正常返回 201 且 DB `users.password` 为 PHC；向 DB 安装 `BEFORE INSERT ON users` 且只拦截该测试账号的错误触发器，再 POST 时返回错误，`GET /api/v1/users` 中**不出现**半创建账号。PATCH 改密码触发 token 删除失败时用户密码/role/会话状态都不应部分改变（在 `app.db` 单事务）。
- [ ] **Step 2 — 验红：** `cargo test -p api --test management user_lifecycle` 中新增的两个故障注入测试应失败。
- [ ] **Step 3 — 实现：** `create_user` 改走 `insert_user_with_password`；`update_user` 带密码时走 `update_user_and_password`，不带时保留 `save_user`；对 `StoreError` 使用已有 `store.error` 信封并记录不带口令的 `tracing::error!`。在同事务先执行全部 SQL，最后 commit。不要改变 `PATCH /users/{id}` 的 API DTO。
- [ ] **Step 4 — 测绿：** `cargo test -p store && cargo test -p api --test management user_lifecycle && cargo test -p api --test management webapi && cargo test -p api --test jellyfin`。
- [ ] **Step 5 — Commit：** `git add crates/store/src/users.rs crates/api/src/http/users.rs crates/api/tests/management/user_lifecycle.rs && git commit -m "fix(users): make credential writes atomic"`。

**出包前复核：** 新 ADMIN_PASSWORD 配置上线方式、旧实例迁移/回滚和 secret 文件权限。运行 `cargo test -p api && cargo test -p store && cargo test --workspace`；前端 `cd web && pnpm exec tsc --noEmit && pnpm test`。未验证生产已有口令升级路径之前不能发布。对不存在用户/错误口令的 Argon2 耗时差异做风险评估：若服务对不可信网络开放，再增固定 dummy PHC 校验与速率限制作为独立任务，不能用不稳定的时间断言替代安全评审。
