# 生产启动管理员密码门禁 Implementation Plan

> **历史状态（2026-10-13 整理）：** 生产管理员密码门禁历史方案；后续 CLI 生命周期/凭据约束方案继续调整旧实例迁移，不授权重跑初始化或轮换。
>
> 本文保留原设计、发现/任务 ID 与当时证据；日期本身不表示过时。旧执行/提交/清理命令仅作历史记录，不应直接重跑；复选框不是当前实现清单。当前依据：[数据库/API review](<../../audits/2026-10-13-database-api-review.md>) 与 [剩余计划](<2026-10-13-database-api-followup.md>)，未覆盖的旧 ID 仍须单独对账。

> **规模规则更新：** 文中的 60 行硬限/整改目标是历史要求；当前 [AGENTS.md](<../../../AGENTS.md>) 为生产函数软限 60、测试软限 80、两者硬限 120。历史测试/行数记录不改写，规模合规不等于功能安全。

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 生产首次启动与不安全旧实例必须提供独立 `CRAWLER_MEDIA_ADMIN_PASSWORD`；测试 fixture 仍可用 CLI token 当密码。轮换管理员密码时同事务完成，且不把 ADMIN_PASSWORD 写入 `user_tokens`。

**Architecture:** 把「是否允许用 CLI token 冒充管理员密码」从 `ApiState::new_arc_with_admin_password` 拆开。生产 `main` 先调用只读 `prepare_admin_credentials`，不满足条件则不写管理员行。测试 `ApiState::new` / `new_arc` 继续走宽松路径，避免 300+ 个 `management-secret` fixture 全部改口令。轮换走 `Store::rotate_admin_password_keeping_cli_token`，在 `app.db` 单事务内改密并重登记 CLI bearer。

**Tech Stack:** Rust 2024、rusqlite 0.37、Argon2id（ADR-0010）、axum 测试 oneshot。

## Global Constraints

- `domain` 纯类型；`store` 不依赖 `api`。
- 登录口令遵循 ADR-0010；Jellyfin `AuthenticateByName` 与 `/api/v1/auth/login` 共用 `Store::verify_user_password`。
- 口令、token、PHC 哈希不进日志；失败只记用户 id / 阶段 / 错误种类。
- 文件硬限 800 行，函数硬限 60 行；测试逻辑不豁免。
- 静态图片端点保持公开。自有 REST 仍是 `/api/v1` `{ok,data}` 信封。

## Product rules

1. 生产 `crawler-media` 二进制：全新安装必须设置 `CRAWLER_MEDIA_ADMIN_PASSWORD`，且不得等于 `CRAWLER_MEDIA_TOKEN`。缺其一或两者相等时，在写入管理员行之前返回启动错误，DB 不变。
2. 已有管理员且当前 CLI token **不能**通过密码校验：无 ADMIN_PASSWORD 照旧启动，不改密码。
3. 已有管理员且当前 CLI token **仍能**通过密码校验：必须提供不同的 ADMIN_PASSWORD；否则启动失败、DB 不变。提供后同事务更新 `users.password`，并 `INSERT OR REPLACE` 当前 CLI token（不删除运维入口）。
4. 测试/开发 `ApiState::new` 与 `new_arc` 仍允许 `admin_password=None` 时用 token 种子密码，因为现有 fixture 的 Bearer 就是 `"management-secret"`。
5. ADMIN_PASSWORD 永不写入 `user_tokens`。

## File map

| File | Responsibility |
|---|---|
| `docs/adr/0010-password-hashing.md` | 写清生产门禁、测试宽松路径、旧备份轮换建议 |
| `crates/store/src/users.rs` | `rotate_admin_password_keeping_cli_token` 单事务改密+重登记 CLI token |
| `crates/api/src/bootstrap_credentials.rs` | 只读判定 + 生产错误文案，不签发 token |
| `crates/api/src/main.rs` | 生产启动调用判定，失败则不创建 `ApiState` |
| `crates/api/src/management/state.rs` | 生产入口不再 `unwrap_or(&token)`；测试入口保持宽松 |
| `crates/api/src/http/users.rs` | 创建/改密失败 `tracing::error!`（不写口令） |
| `crates/api/tests/bootstrap_credentials.rs` | 生产门禁与旧实例轮换 |
| `crates/api/tests/management/user_lifecycle.rs` | INSERT/PATCH 故障注入 |
| `crates/store/tests/password_migration.rs` | 轮换事务失败回滚 |

---

### Task 1: 生产启动拒绝缺密码或密码等于 token

**Files:** Create `crates/api/src/bootstrap_credentials.rs`; Modify `crates/api/src/lib.rs`, `crates/api/src/main.rs`, `crates/api/src/management/state.rs`, `docs/adr/0010-password-hashing.md`; Test `crates/api/tests/bootstrap_credentials.rs`.

**Interfaces:**
- Produces: `pub enum BootstrapMode { Production, TestFixture }`
- Produces: `pub fn prepare_admin_credentials(store: &Store, token: &str, admin_password: Option<&str>, mode: BootstrapMode) -> Result<AdminBootstrapAction, BootstrapCredentialsError>`
- Produces: `pub enum AdminBootstrapAction { Seed { password: String, cli_token: String }, Rotate { password: String, cli_token: String }, Keep }`
- Produces: `BootstrapCredentialsError { MissingAdminPassword, PasswordEqualsToken, InsecureLegacyNeedsPassword }`，`Display` 不含 secret。
- Consumes: `Store::get_user`, `Store::password_matches_without_session`（已存在，只读）。

- [ ] **Step 1 — 红测：** 在 `bootstrap_credentials.rs` 增加：

```rust
#[test]
fn production_fresh_install_rejects_missing_admin_password_before_any_user_row() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let err = prepare_admin_credentials(&store, "cli-token", None, BootstrapMode::Production).unwrap_err();
    assert!(matches!(err, BootstrapCredentialsError::MissingAdminPassword));
    assert!(store.get_user(UserId::from_str("00000000-0000-0000-0000-000000000001").unwrap()).unwrap().is_none());
}

#[test]
fn production_fresh_install_rejects_equal_secrets() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let err = prepare_admin_credentials(&store, "same", Some("same"), BootstrapMode::Production).unwrap_err();
    assert!(matches!(err, BootstrapCredentialsError::PasswordEqualsToken));
}

#[test]
fn test_fixture_mode_may_reuse_token_as_password() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let action = prepare_admin_credentials(&store, "management-secret", None, BootstrapMode::TestFixture).unwrap();
    match action {
        AdminBootstrapAction::Seed { password, cli_token } => {
            assert_eq!(password, "management-secret");
            assert_eq!(cli_token, "management-secret");
        }
        _ => panic!("fixture must seed"),
    }
}
```

- [ ] **Step 2 — 验红：** `cargo test -p api --test bootstrap_credentials production_fresh_install_rejects_missing`。当前 `main.rs` 缺密码不报错，新函数尚不存在。
- [ ] **Step 3 — 实现：** `prepare_admin_credentials`：查固定管理员 id `00000000-0000-0000-0000-000000000001`。无用户且 `mode=Production`：`admin_password` 空 → `MissingAdminPassword`；等于 token → `PasswordEqualsToken`；否则 `Seed`。无用户且 `mode=TestFixture`：密码缺省用 token，仍 `Seed`。已有用户：`password_matches_without_session("admin", token)` 为真且 `mode=Production` 且未提供不同密码 → `InsecureLegacyNeedsPassword`；提供不同密码 → `Rotate`；密码已独立 → `Keep`。`main.rs` 在 `ApiState::new_arc_with_admin_password` **之前**调用 Production 模式；失败 `return Err(Box::new(err))`。`new_arc_with_admin_password` 删除 `unwrap_or(&token)`，改为接收已判定的 `AdminBootstrapAction`，或内部对 `None` 仅在测试路径使用。推荐签名改为：

```rust
pub fn new_arc_with_admin_bootstrap<F>(
    store: Arc<Mutex<Store>>,
    profiles: ProfileSet,
    fetcher: Arc<F>,
    downloader: Arc<dyn Downloader>,
    library_root: PathBuf,
    action: AdminBootstrapAction,
) -> Result<Self, ApiStateError>
where
    F: Fetcher + 'static;
```

`new_arc` 先 `prepare_admin_credentials(store, &token, None, BootstrapMode::TestFixture)` 再调用。`new_arc_with_admin_password(store, token, admin_password, profiles, fetcher, downloader, library_root)` 保留给现有测试：`Some(pw)` 走 `BootstrapMode::Production`，`None` 走 `TestFixture`。生产 `main` 只调用 `prepare_admin_credentials(..., Production)` + `new_arc_with_admin_bootstrap`。

- [ ] **Step 4 — 测绿：** `cargo test -p api --test bootstrap_credentials && cargo test -p api --test management user_lifecycle && cargo test -p api --test jellyfin authenticate_by_name`
- [ ] **Step 5 — Commit：** `git add docs/adr/0010-password-hashing.md crates/api/src/bootstrap_credentials.rs crates/api/src/lib.rs crates/api/src/main.rs crates/api/src/management/state.rs crates/api/tests/bootstrap_credentials.rs && git commit -m "fix(auth): require distinct production admin password"`

### Task 2: 旧实例轮换同事务且 fail-closed

**Files:** Modify `crates/store/src/users.rs`, `crates/api/src/management/state.rs`; Test `crates/store/tests/password_migration.rs`, `crates/api/tests/bootstrap_credentials.rs`.

**Interfaces:**
- Produces: `Store::rotate_admin_password_keeping_cli_token(user_id: UserId, new_password: &str, cli_token: &str) -> Result<(), StoreError>`
- 同事务：`UPDATE users.password`（先 `hash_password`）、`INSERT OR REPLACE INTO user_tokens (token,user_id,created_at)`。不要 `DELETE` 当前 CLI token 后再插入。
- `Seed` 路径继续 `set_password` + `set_user_token`，且 `set_user_token` 的值是 CLI token，不是 admin password。

- [ ] **Step 1 — 红测：**

```rust
#[test]
fn rotate_admin_password_rolls_back_when_token_insert_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let id = UserId::from_str("00000000-0000-0000-0000-000000000001").unwrap();
    store.insert_user(&User { id, login: "admin".into(), enabled: true, role: UserRole::Admin }).unwrap();
    store.set_password(id, "old-token").unwrap();
    store.set_user_token(id, "old-token").unwrap();
    let conn = rusqlite::Connection::open(tmp.path().join("app.db")).unwrap();
    conn.execute(
        "CREATE TRIGGER reject_token BEFORE INSERT ON user_tokens BEGIN SELECT RAISE(FAIL, 'no token'); END;",
        [],
    ).unwrap();
    let err = store.rotate_admin_password_keeping_cli_token(id, "new-admin-pass", "old-token");
    assert!(err.is_err());
    assert!(store.password_matches_without_session("admin", "old-token").unwrap());
    assert!(!store.password_matches_without_session("admin", "new-admin-pass").unwrap());
}
```

再补 HTTP：不安全旧实例无新密码时 `prepare_admin_credentials` 返回 `InsecureLegacyNeedsPassword`；有新密码后 `verify_user_password("admin", new)` 成功、`"old-token"` 不能密码登录、`user_tokens` 仍含 CLI token。

- [ ] **Step 2 — 验红：** `cargo test -p store --test password_migration rotate_admin_password_rolls_back`
- [ ] **Step 3 — 实现：** `rotate_admin_password_keeping_cli_token` 先哈希，再 `unchecked_transaction`：UPDATE password、INSERT OR REPLACE token、commit。`state.rs` 的 `Rotate` 分支调用它，删除 `let _ = delete_token`。哈希失败 `StoreError::PasswordHash` + `tracing::error!(user_id=%id, error=%e, "管理员密码轮换哈希失败")`。
- [ ] **Step 4 — 测绿：** `cargo test -p store --test password_migration && cargo test -p api --test bootstrap_credentials`
- [ ] **Step 5 — Commit：** `git add crates/store/src/users.rs crates/store/tests/password_migration.rs crates/api/src/management/state.rs crates/api/tests/bootstrap_credentials.rs && git commit -m "fix(auth): rotate legacy admin password in one transaction"`

### Task 3: 用户写入失败可观测且不留半行

**Files:** Modify `crates/api/src/http/users.rs`; Test `crates/api/tests/management/user_lifecycle.rs`.

**Interfaces:** 保持 `POST /api/v1/users` 与 `PATCH /api/v1/users/{id}` DTO。`insert_user_with_password` / `update_user_and_password` 已存在。

- [ ] **Step 1 — 红测：** `create_user_rolls_back_when_password_insert_is_rejected`：对 `app.db` 安装 `BEFORE INSERT ON users WHEN NEW.login='boom' BEGIN SELECT RAISE(FAIL,'nope'); END;`，`POST /api/v1/users {login:boom,password:x}` 返回 500 `store.error`，`GET /api/v1/users` 无 `boom`。`patch_password_rolls_back_when_token_delete_fails`：先创建 alice 并登录，安装 `BEFORE DELETE ON user_tokens BEGIN SELECT RAISE(FAIL,'keep'); END;`，PATCH 改密返回 500，旧 token 仍可 `GET /api/v1/auth/me`，新密码不能登录。
- [ ] **Step 2 — 验红：** `cargo test -p api --test management user_lifecycle create_user_rolls_back`
- [ ] **Step 3 — 实现：** `create_user`/`update_user` 的 `Err(error)` 分支加 `tracing::error!(%error, user_id=?user.id, "用户凭据写入失败")`，不记录 password。逻辑已走原子方法则测试应变绿。
- [ ] **Step 4 — 测绿：** `cargo test -p api --test management user_lifecycle && cargo test -p api --test users`
- [ ] **Step 5 — Commit：** `git add crates/api/src/http/users.rs crates/api/tests/management/user_lifecycle.rs && git commit -m "fix(users): log credential write failures without leaking secrets"`

**出包前：** ADR-0010 写明：历史备份里的明文 CLI token 不可逆，操作者应轮换 `CRAWLER_MEDIA_TOKEN` 并销毁旧备份。`cargo test -p store && cargo test -p api --test bootstrap_credentials && cargo test --workspace`。
