# 管理员 CLI 凭据生命周期与部署修复 Implementation Plan

> **历史状态（2026-10-13 整理）：** CLI bearer 生命周期历史方案；安全边界和一次性迁移决策保留，本次未重验现存实例凭据升级。
>
> 本文保留原设计、发现/任务 ID 与当时证据；日期本身不表示过时。旧执行/提交/清理命令仅作历史记录，不应直接重跑；复选框不是当前实现清单。当前依据：[数据库/API review](<../../audits/2026-10-13-database-api-review.md>) 与 [剩余计划](<2026-10-13-database-api-followup.md>)，未覆盖的旧 ID 仍须单独对账。

> **规模规则更新：** 文中的 60 行硬限/整改目标是历史要求；当前 [AGENTS.md](<../../../AGENTS.md>) 为生产函数软限 60、测试软限 80、两者硬限 120。历史测试/行数记录不改写，规模合规不等于功能安全。

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 更换 CLI token 时立刻撤销旧值；同时更换旧 CLI token 与管理员密码时不能留下旧口令；管理员 UI 改密不使 CLI 永久失效；新安装部署示例能启动。

**Architecture:** 系统 CLI Bearer 与 30 天用户会话分离，而不是只靠普通 `user_tokens.created_at` 猜哪个 token 是 CLI。用 `app.db` 持久化唯一的当前 CLI token 身份（在 `settings` 表以受控 key 保存，避免第二个 DB），启动在单事务中撤销旧 token、登记新 token、同步当前管理员密码；鉴于旧数据库无法可靠分辨 CLI 和管理员会话，首次迁移必须显式处理不确定性而不能悄悄保留旧 Bearer。用户主动改密只撤销普通会话，不撤销 CLI。旧管理员凭据判断同时考虑新环境变量及现存 CLI 候选，而不是只比较新 token。

**Tech Stack:** Rust 2024、rusqlite 0.37、argon2、Axum 0.8、Docker Compose。

## Global Constraints

- CLI token 已在 `user_tokens.token` 以明文存储；本计划不承诺保护数据库泄露下的 bearer，相关边界见 ADR-0010。`settings` 若记录 CLI token 同样是 secret，运维备份保持机密。
- 正常用户会话继续按 `SESSION_TTL_SECS` 过期；CLI 的生命周期由配置与明确撤销决定，不能被会话 TTL 意外中断。
- `AGENTS.md`：文件 ≤800 行、函数/测试 ≤60 行；每 task 先红测再绿；错误日志不包含密码、CLI token 或 PHC。
- 自有 `/api/v1` 和 Jellyfin 都使用同一 token 解析；静态图片保持公开。
- 首次未知历史 CLI 值无法安全指认：选用**一次性的管理员会话全撤销**，明确在部署文档提示 admin 需重新登录；成员会话不受影响。防止由历史未知 token 长达 30 天保持管理员权限。

## File map

| File | Responsibility |
|---|---|
| `crates/store/src/users.rs` | 事务式设置/轮换 CLI token，管理员改密保留 CLI，历史候选检测，查询 CLI token 单独适用会话 TTL 规则 |
| `crates/store/src/schema.rs` | 若增加 CLI 身份 marker，保证老库升级与默认值安全（优先既有 `settings` KV，避免新表） |
| `crates/api/src/bootstrap_credentials.rs` | 只读分析旧账号及旧 token、需要迁移时 fail closed |
| `crates/api/src/management/state.rs` | 启动统一调用 store 单事务 API，不分别插入或续期 token |
| `crates/api/src/http/auth.rs` | CLI bearer 的 logout 不破坏配置凭据；普通会话注销仍有效 |
| `crates/api/src/http/users.rs` | 管理员密码更新使用保留 CLI 的事务路径 |
| `crates/store/tests/password_migration.rs` | 事务回滚、普通会话隔离、CLI TTL 及单一身份测试 |
| `crates/api/tests/bootstrap_credentials.rs`, `crates/api/tests/management/user_lifecycle.rs`, `crates/api/tests/jellyfin.rs` | 配置轮换、旧库升级与两套 HTTP 协议回归 |
| `crates/api/src/config.rs`, `docker-compose.yml`, `docs/deploy.md`, `start-test.sh`, `docs/adr/0010-password-hashing.md` | 新部署 secret、迁移说明和运维行为 |

---

### Task 1: CLI token 只允许当前值，轮换立刻撤销旧值

**Files:** Modify `crates/store/src/users.rs`, `crates/api/src/management/state.rs`; Test `crates/store/tests/password_migration.rs`, `crates/api/tests/bootstrap_credentials.rs`, `crates/api/tests/jellyfin.rs`.

**Interfaces:** `Store::register_current_cli_token(admin_id: UserId, token: &str) -> Result<(), StoreError>`。事务内读 `settings.key='auth.cli_token.current'`，若旧值存在且不等于新值则删除其 `user_tokens` 行；无 marker（旧库）时在事务中 `DELETE FROM user_tokens WHERE user_id=?` 清理历史未知管理员 bearer；然后 `INSERT OR REPLACE INTO user_tokens(token,user_id,created_at)` 新值，写 marker，提交。marker 只能用于固定管理员 id，不受同名成员影响。`Keep` 必须调用此方法，不调用旧 `set_user_token`。`Store::user_by_token` 对当前 marker 对应且属于固定管理员的 CLI token 允许超过普通 `SESSION_TTL_SECS`，其它 token 仍检查 TTL；不得通过任意 `user_tokens` 长效化。

- [ ] **Step 1 — 红测：** 初始 token A/密码 P，重启 token B/密码 P：`/api/v1/auth/me` 与 Jellyfin `/Items` 对 Bearer B 返回成功，对 A 立刻 401；独立成员的 token 仍有效。把当前 CLI B 的 `created_at` 手动改到普通 TTL 之外，B 仍有效；普通会话 S 改到同一旧时间后必须 401。若注入 `BEFORE INSERT ON user_tokens RAISE(FAIL)`，旋转返回 `Err`，A 仍有效，marker 仍为 A；撤去触发器后重试可成功。旧库 marker 缺失时给 admin 预存 A 和普通会话 UUID，重新登记 B 后二者都失效，普通成员 token 保留。
- [ ] **Step 2 — 验红：** `cargo test -p api --test bootstrap_credentials cli_token_rotation_revokes_old && cargo test -p store --test password_migration cli_registration_rollback`；当前 A 保留。
- [ ] **Step 3 — 最小实现：** 在 Store 新增上述事务方法，不以 `INSERT OR REPLACE` 单独完成轮换；marker 的读/写、撤旧、写新必须同事务。`Seed` 在同事务写用户+token+marker（扩展现有 `seed_admin_with_credentials`）；`Rotate` 同事务改密+撤旧+写新+marker（扩展 `rotate_admin_password_keeping_cli_token`）。`Keep` 只调用 `register_current_cli_token`。如果 marker 的值已经等于当前值，不要清理 admin 普通会话。
- [ ] **Step 4 — 测绿：** `cargo test -p store && cargo test -p api --test bootstrap_credentials && cargo test -p api --test jellyfin`。
- [ ] **Step 5 — Commit：** `git add crates/store/src/users.rs crates/api/src/management/state.rs crates/store/tests/password_migration.rs crates/api/tests/bootstrap_credentials.rs crates/api/tests/jellyfin.rs && git commit -m "fix(auth): revoke old CLI bearer atomically on rotation"`

### Task 2: 同时变更 CLI 值与旧管理员密码不能逃过迁移

**Files:** Modify `crates/api/src/bootstrap_credentials.rs`, `crates/store/src/users.rs`; Test `crates/api/tests/bootstrap_credentials.rs`.

**Interfaces:** `Store::legacy_admin_password_matches_known_token(admin_id: UserId, current_cli: &str) -> Result<bool, StoreError>`：只读查询 `users.password` 与固定管理员已记录的 `auth.cli_token.current`、`user_tokens` 中现存的管理员 token 候选，比对已存 PHC 或旧明文；不得调用会签发会话的 `verify_user_password`。`prepare_admin_credentials` 对当前 token 或任何上述旧候选匹配密码时，要求新的、不同的 ADMIN_PASSWORD。若新安装或 DB 中管理员密码为空/null，也不得误判为“已有安全管理员”；走显式恢复/首次设置路径。

- [ ] **Step 1 — 红测：** 创建旧库 `admin.password = "old-cli-A"`（旧明文与 Argon2id 已升级版本分别测）、管理员 token 行 `"old-cli-A"`，marker 缺失。以环境 TOKEN=B、ADMIN_PASSWORD=C 启动：必须返回 `Rotate`，B Bearer 可用，A 不能作为密码登录或 Bearer；缺 C 时拒绝启动且 DB 无写。已有独立密码 P 但同时更新 TOKEN=B、ADMIN_PASSWORD=C：仍 `Keep`，P 不被覆盖。
- [ ] **Step 2 — 验红：** `cargo test -p api --test bootstrap_credentials simultaneous_cli_and_admin_password_change`；当前 `password_matches_without_session("admin", B)` 为 false 返回 `Keep`。
- [ ] **Step 3 — 实现：** 对当前与旧候选执行**只读**口令对照（空串不做认证），匹配即 `Rotate` 或 `InsecureLegacyNeedsPassword`；同任务验证历史候选不会把普通 UUID 会话误判成密码，查询错误向上传播而非默认安全；调用 Task 1 的事务轮换保持所有变更原子。迁移成功后不再把旧明文存入 `settings`。
- [ ] **Step 4 — 测绿：** `cargo test -p store && cargo test -p api --test bootstrap_credentials`。
- [ ] **Step 5 — Commit：** `git add crates/api/src/bootstrap_credentials.rs crates/store/src/users.rs crates/api/tests/bootstrap_credentials.rs && git commit -m "fix(auth): detect legacy password when CLI token changes"`

### Task 3: 管理员改密与会话注销不破坏 CLI

**Files:** Modify `crates/store/src/users.rs`, `crates/api/src/http/users.rs`, `crates/api/src/http/auth.rs`; Test `crates/api/tests/management/user_lifecycle.rs`, `crates/store/tests/password_migration.rs`.

**Interfaces:** 在 `Store::update_user_and_password` 内若 `user.id` 是固定管理员：事务先从 marker 读 CLI token，再 `DELETE FROM user_tokens WHERE user_id=? AND token <> ?`，即只撤销普通会话；成员仍 `DELETE WHERE user_id=?`。`Store::delete_token(&str)` 对 marker 对应的 CLI bearer 不执行删除（返回 `Ok(false)`），普通 token 继续删除，`logout` 仍清 cookie。若管理员 CLI token 丢失，应由启动事务恢复，不通过把 token 当密码登录修复。

- [ ] **Step 1 — 红测：** 管理员 Bearer CLI=B、登录会话 S；以 CLI 或 S PATCH 管理员密码到 P2：S 应立即 401，B 仍 200，P2 可登录，旧密码不能登录。用 CLI=B 请求 logout，CLI 仍 200；用普通 S2 logout，S2 失效；成员改密所有成员会话失效。注入删除 token 触发器，PATCH 出错时密码和各 token 都不变。
- [ ] **Step 2 — 验红：** `cargo test -p api --test management user_lifecycle admin_password_patch_keeps_cli`；当前删除了 CLI token。
- [ ] **Step 3 — 实现：** 在 store 同事务保护 CLI marker + token，普通 session 和 CLI 明确分支；确保不要由请求头把 CLI token 落到 session cookie 或输出日志；HTTP 用户 DTO 不变。对外 `logout` 若只传 CLI Bearer 可返回既有幂等成功，但不撤销配置凭据。
- [ ] **Step 4 — 测绿：** `cargo test -p store && cargo test -p api --test management user_lifecycle && cargo test -p api --test jellyfin`。
- [ ] **Step 5 — Commit：** `git add crates/store/src/users.rs crates/api/src/http/users.rs crates/api/src/http/auth.rs crates/api/tests/management/user_lifecycle.rs crates/store/tests/password_migration.rs && git commit -m "fix(auth): preserve configured CLI credential across password changes"`

### Task 4: 对齐新安装部署入口

**Files:** Modify `docker-compose.yml`, `docs/deploy.md`, `start-test.sh`, `docs/adr/0010-password-hashing.md`.

**Interfaces:** 生产新安装必须同时传 `CRAWLER_MEDIA_TOKEN` 与不同的 `CRAWLER_MEDIA_ADMIN_PASSWORD`；支持 `_FILE`。Compose 不应再给 TOKEN 回退到 `changeme`，`ADMIN_PASSWORD` 缺少值要在启动前清晰失败。`start-test.sh` 可为本地测试明确传一个**不同于** `TOKEN` 的 `ADMIN_PASSWORD`（可经环境覆盖），绝不直接将 TOKEN 赋给 ADMIN_PASSWORD。

- [ ] **Step 1 — 红测：** 脚本/文档合同检查：Compose `docker compose config` 在设置 `CRAWLER_MEDIA_TOKEN=cli-test CRAWLER_MEDIA_ADMIN_PASSWORD=admin-test` 时包含两个不同值；不设 ADMIN_PASSWORD 时 compose 不再隐式给 `changeme`；`start-test.sh` 的 backend env 含独立 ADMIN_PASSWORD。用 tempdir 跑不启动真实服务器的配置检查，检查未设置时进程返回明确错误且 `users` 表无 admin 行。
- [ ] **Step 2 — 验红：** `docker compose config`（或缺 Docker 时用 `python3` 解析变量文本），当前 Compose/脚本没有 ADMIN_PASSWORD。
- [ ] **Step 3 — 实现：** Compose 加 `CRAWLER_MEDIA_ADMIN_PASSWORD: ${CRAWLER_MEDIA_ADMIN_PASSWORD:-}` 与 `CRAWLER_MEDIA_TOKEN: ${CRAWLER_MEDIA_TOKEN:-}`，并透传 `${CRAWLER_MEDIA_ADMIN_PASSWORD_FILE:-}` / `${CRAWLER_MEDIA_TOKEN_FILE:-}`；不在 Compose 用 `:?` 强制直接值，否则 `_FILE`-only 部署会在 Compose 解析时被拒。由服务端 `secret()` + 生产门禁检查非空/不同；文档命令在 Compose 前显式 `export` 两个不同值。文档本地 run、Compose 命令、登录说明更新为 `admin / $CRAWLER_MEDIA_ADMIN_PASSWORD`；列出旧备份、首次升级时管理员会话重新登录、TOKEN 轮换及 30 天普通 session 边界。
- [ ] **Step 4 — 验绿：** `cargo test -p api --test bootstrap_credentials && docker compose config`（若 docker 可用）及 `git diff --check`。
- [ ] **Step 5 — Commit：** `git add docker-compose.yml docs/deploy.md start-test.sh docs/adr/0010-password-hashing.md && git commit -m "docs(deploy): require separate production admin secret"`

**整体回归：** `cargo test -p store && cargo test -p api && cargo test --workspace`。测试 fixtures 的 `ApiState::new` 宽松模式保持，仅生产 main 强制不同口令；固定 `CRAWLER_MEDIA_TOKEN` 不能冒充登录密码。
