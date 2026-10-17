use std::str::FromStr;

use domain::{User, UserId, UserRole};
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

use super::{Store, StoreError};

/// 会话有效期（秒）：签发后 30 天内有效，过期必须重新登录。
pub const SESSION_TTL_SECS: i64 = 30 * 86_400;
pub const SETTING_CLI_TOKEN: &str = "auth.cli_token.current";

fn now_unix() -> i64 {
    super::unix_now()
}

fn map_user(row: &rusqlite::Row<'_>) -> rusqlite::Result<User> {
    let role: String = row.get(3)?;
    Ok(User {
        id: super::maps::parse_id(row.get(0)?, 0)?,
        login: row.get(1)?,
        enabled: row.get(2)?,
        role: UserRole::from_str(&role).unwrap_or(UserRole::Member),
    })
}

/// 密码校验所需的登录快照：在持有 Store 锁时读取，随后在锁外完成 Argon2 校验。
/// 不实现 `Debug`/`Serialize`，避免口令材料进入日志或响应。
pub struct LoginCredential {
    pub user_id: UserId,
    pub observed_password: String,
}

impl Store {
    pub fn insert_user(&self, user: &User) -> Result<(), StoreError> {
        self.app.execute(
            "INSERT INTO users (id, login, enabled, role) VALUES (?1, ?2, ?3, ?4)",
            params![
                user.id.to_string(),
                user.login,
                user.enabled,
                user.role.as_str()
            ],
        )?;
        Ok(())
    }

    /// 先计算哈希，并在单一事务中原子插入用户及其密码；失败则全部回滚。
    pub fn insert_user_with_password(&self, user: &User, password: &str) -> Result<(), StoreError> {
        let hashed = crate::password::hash_password(password).map_err(StoreError::PasswordHash)?;
        let tx = self.app.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO users (id, login, password, enabled, role) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                user.id.to_string(),
                user.login,
                hashed,
                user.enabled,
                user.role.as_str()
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// 在单一事务中原子更新用户信息、更新哈希密码并撤销历史会话 Token（系统当前配置的 CLI token 保留）。
    pub fn update_user_and_password(&self, user: &User, password: &str) -> Result<(), StoreError> {
        let hashed = crate::password::hash_password(password).map_err(StoreError::PasswordHash)?;
        let tx = self.app.unchecked_transaction()?;
        let cli_token: Option<String> = tx
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![SETTING_CLI_TOKEN],
                |row| row.get(0),
            )
            .optional()?;
        if user.role == UserRole::Admin && cli_token.as_deref() == Some(password) {
            return Err(StoreError::CredentialConflict(
                "管理员密码不得与当前配置的系统 CLI Token 相同".into(),
            ));
        }
        tx.execute(
            "UPDATE users SET login = ?1, enabled = ?2, role = ?3, password = ?4 WHERE id = ?5",
            params![
                user.login,
                user.enabled,
                user.role.as_str(),
                hashed,
                user.id.to_string()
            ],
        )?;
        if let Some(cli) = cli_token {
            tx.execute(
                "DELETE FROM user_tokens WHERE user_id = ?1 AND token <> ?2",
                params![user.id.to_string(), cli],
            )?;
        } else {
            tx.execute(
                "DELETE FROM user_tokens WHERE user_id = ?1",
                params![user.id.to_string()],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// "admin" | "member" — defaults to member for rows without a role.
    pub fn user_role(&self, id: UserId) -> Result<String, StoreError> {
        self.role_lookups.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(self
            .app
            .query_row(
                "SELECT role FROM users WHERE id = ?1",
                params![id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .unwrap_or_else(|| "member".into()))
    }

    pub fn get_user(&self, id: UserId) -> Result<Option<User>, StoreError> {
        self.app
            .query_row(
                "SELECT id, login, enabled, COALESCE(role, 'member') FROM users WHERE id = ?1",
                params![id.to_string()],
                map_user,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn list_users(&self) -> Result<Vec<User>, StoreError> {
        let mut stmt = self.app.prepare(
            "SELECT id, login, enabled, COALESCE(role, 'member') FROM users ORDER BY login",
        )?;
        let rows = stmt.query_map([], map_user)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// 强制设置用户角色为管理员（种子初始化使用）。
    pub fn force_admin_role(&self, user_id: UserId) -> Result<(), StoreError> {
        self.app.execute(
            "UPDATE users SET role = 'admin' WHERE id = ?1",
            params![user_id.to_string()],
        )?;
        Ok(())
    }

    pub fn set_user_token(&self, user_id: UserId, token: &str) -> Result<(), StoreError> {
        self.app.execute(
            "INSERT OR REPLACE INTO user_tokens (token, user_id, created_at) VALUES (?1, ?2, ?3)",
            params![token, user_id.to_string(), now_unix()],
        )?;
        Ok(())
    }

    /// 登记当前配置的 CLI Bearer Token，并在同一事务中撤销被替换的旧 CLI token。
    /// 保证系统只有一个当前有效配置的 CLI 凭据，轮换后旧值立即失效。
    pub fn register_current_cli_token(
        &self,
        admin_id: UserId,
        token: &str,
    ) -> Result<(), StoreError> {
        let tx = self.app.unchecked_transaction()?;
        apply_cli_token_tx(&tx, admin_id, token)?;
        tx.commit()?;
        Ok(())
    }

    /// 在单一事务中原子插入种子管理员（id、login、password、role=admin）并登记 CLI Token。
    /// 任何一步失败整个事务完整回滚，绝不残留无密码或无 Token 的半成状态。
    pub fn seed_admin_with_credentials(
        &self,
        user_id: UserId,
        password: &str,
        cli_token: &str,
    ) -> Result<(), StoreError> {
        let hashed = crate::password::hash_password(password).map_err(|e| {
            tracing::error!(%user_id, error = %e, "种子管理员密码哈希失败");
            StoreError::PasswordHash(e)
        })?;
        let tx = self.app.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO users (id, login, password, enabled, role) VALUES (?1, 'admin', ?2, 1, 'admin')
             ON CONFLICT(id) DO UPDATE SET password = excluded.password, enabled = 1, role = 'admin'",
            params![user_id.to_string(), hashed],
        )?;
        tx.execute(
            "DELETE FROM user_tokens WHERE user_id = ?1",
            params![user_id.to_string()],
        )?;
        apply_cli_token_tx(&tx, user_id, cli_token)?;
        tx.commit()?;
        Ok(())
    }

    /// 检查管理员是否已经设置过非空密码。用于启动自检防止空口令被误判为安全管理员。
    pub fn admin_password_is_set(&self, admin_id: UserId) -> Result<bool, StoreError> {
        let stored_pw: Option<Option<String>> = self
            .app
            .query_row(
                "SELECT password FROM users WHERE id = ?1",
                params![admin_id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        Ok(stored_pw.flatten().is_some_and(|pw| !pw.trim().is_empty()))
    }

    /// 在同一个事务中原子轮换管理员密码，并确保当前的 CLI token 登记有效且旧 CLI Bearer 被撤销。
    /// 若过程中出错则全部回滚，fail-closed，避免产生密码更新但凭据丢失的半边状态。
    pub fn rotate_admin_password_keeping_cli_token(
        &self,
        user_id: UserId,
        new_password: &str,
        cli_token: &str,
    ) -> Result<(), StoreError> {
        let hashed = crate::password::hash_password(new_password).map_err(|e| {
            tracing::error!(%user_id, error = %e, "管理员密码轮换哈希失败");
            StoreError::PasswordHash(e)
        })?;
        let tx = self.app.unchecked_transaction()?;
        tx.execute(
            "UPDATE users SET password = ?1 WHERE id = ?2",
            params![hashed, user_id.to_string()],
        )?;
        tx.execute(
            "DELETE FROM user_tokens WHERE user_id = ?1",
            params![user_id.to_string()],
        )?;
        apply_cli_token_tx(&tx, user_id, cli_token)?;
        tx.commit()?;
        Ok(())
    }

    /// 校验密码是否匹配，但绝不写入数据库，也不签发会话 token（用于启动自检与安全迁移判定）。
    pub fn password_matches_without_session(
        &self,
        login: &str,
        password: &str,
    ) -> Result<bool, StoreError> {
        if password.is_empty() {
            return Ok(false);
        }
        let user_info = self
            .app
            .query_row(
                "SELECT password FROM users WHERE login = ?1 AND enabled = 1",
                params![login],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?;
        let Some(Some(stored_pw)) = user_info else {
            return Ok(false);
        };
        Ok(crate::password::verify_password(password, &stored_pw))
    }

    /// 检查拟设置的管理员密码是否与任何已知作为系统 CLI Bearer 的密钥重合。
    /// 防止运维在初始化或迁移时将历史 CLI secret 重新用作管理员登录口令。
    pub fn admin_password_matches_known_bearer(
        &self,
        admin_id: UserId,
        proposed: &str,
        current_cli: &str,
    ) -> Result<bool, StoreError> {
        if proposed.is_empty() {
            return Ok(false);
        }
        if proposed == current_cli {
            return Ok(true);
        }
        let old_marker: Option<String> = self
            .app
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![SETTING_CLI_TOKEN],
                |row| row.get(0),
            )
            .optional()?;
        if old_marker.as_deref() == Some(proposed) {
            return Ok(true);
        }
        let mut stmt = self
            .app
            .prepare("SELECT token FROM user_tokens WHERE user_id = ?1")?;
        let tokens: Vec<String> = stmt
            .query_map(params![admin_id.to_string()], |row| row.get(0))?
            .collect::<Result<Vec<String>, _>>()?;
        Ok(tokens.iter().any(|tok| tok == proposed))
    }

    pub fn tokens_for_user(&self, user_id: UserId) -> Result<Vec<String>, StoreError> {
        let mut stmt = self
            .app
            .prepare("SELECT token FROM user_tokens WHERE user_id = ?1")?;
        let rows = stmt.query_map(params![user_id.to_string()], |row| row.get(0))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// 校验管理员当前密码是否与已知系统 CLI token（包含当前传入值、历史 marker 及现有 user_tokens）相同。
    /// 用于升级自检：防止运维同时修改 CLI token 和密码时旧的 token 密码逃过升级迁移。
    pub fn legacy_admin_password_matches_known_token(
        &self,
        admin_id: UserId,
        current_cli: &str,
    ) -> Result<bool, StoreError> {
        let login: Option<String> = self
            .app
            .query_row(
                "SELECT login FROM users WHERE id = ?1 AND enabled = 1",
                params![admin_id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        let Some(login) = login else {
            return Ok(false);
        };
        if self.password_matches_without_session(&login, current_cli)? {
            return Ok(true);
        }
        let old_marker: Option<String> = self
            .app
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![SETTING_CLI_TOKEN],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(old) = old_marker {
            if self.password_matches_without_session(&login, &old)? {
                return Ok(true);
            }
        }
        let mut stmt = self
            .app
            .prepare("SELECT token FROM user_tokens WHERE user_id = ?1")?;
        let tokens: Vec<String> = stmt
            .query_map(params![admin_id.to_string()], |row| row.get(0))?
            .filter_map(Result::ok)
            .collect();
        for tok in tokens {
            if self.password_matches_without_session(&login, &tok)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// 撤销一个会话 token（登出）。系统当前配置的 CLI token 不予物理删除。
    pub fn delete_token(&self, token: &str) -> Result<bool, StoreError> {
        let cli_token: Option<String> = self
            .app
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![SETTING_CLI_TOKEN],
                |row| row.get(0),
            )
            .optional()?;
        if cli_token.as_deref() == Some(token) {
            return Ok(false);
        }
        let n = self
            .app
            .execute("DELETE FROM user_tokens WHERE token = ?1", params![token])?;
        Ok(n > 0)
    }

    /// 在单一事务中原子撤销传入的会话 Token 列表（系统当前配置的 CLI token 不予物理删除）。
    /// 任何一个删除失败则整体回滚，绝不残留部分撤销的不一致状态。
    pub fn delete_session_tokens(&self, tokens: &[String]) -> Result<usize, StoreError> {
        if tokens.is_empty() {
            return Ok(0);
        }
        let cli_token: Option<String> = self
            .app
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![SETTING_CLI_TOKEN],
                |row| row.get(0),
            )
            .optional()?;
        let tx = self.app.unchecked_transaction()?;
        let mut deleted = 0;
        for token in tokens {
            if cli_token.as_deref() == Some(token) {
                continue;
            }
            deleted += tx.execute("DELETE FROM user_tokens WHERE token = ?1", params![token])?;
        }
        tx.commit()?;
        Ok(deleted)
    }

    /// 撤销某用户的全部会话（改密 / 删除用户时调用）。
    pub fn delete_tokens_for_user(&self, user_id: UserId) -> Result<usize, StoreError> {
        let n = self.app.execute(
            "DELETE FROM user_tokens WHERE user_id = ?1",
            params![user_id.to_string()],
        )?;
        Ok(n)
    }

    /// 设置用户密码（凭据存 users 表，与会话 token 分离；改密即旧密码失效）。
    /// 所有输入密码均通过 Argon2id 加盐哈希后存储（ADR-0010），严禁按前缀跳过哈希。
    pub fn set_password(&self, user_id: UserId, password: &str) -> Result<(), StoreError> {
        let value_to_store =
            crate::password::hash_password(password).map_err(StoreError::PasswordHash)?;
        self.app.execute(
            "UPDATE users SET password = ?1 WHERE id = ?2",
            params![value_to_store, user_id.to_string()],
        )?;
        Ok(())
    }

    pub fn save_user(&self, user: &User) -> Result<(), StoreError> {
        let tx = self.app.unchecked_transaction()?;
        tx.execute(
            "UPDATE users SET login = ?1, enabled = ?2, role = ?3 WHERE id = ?4",
            params![
                user.login,
                user.enabled,
                user.role.as_str(),
                user.id.to_string()
            ],
        )?;
        if !user.enabled {
            tx.execute(
                "DELETE FROM user_tokens WHERE user_id = ?1",
                params![user.id.to_string()],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn admin_user_id(&self) -> Result<UserId, StoreError> {
        let raw = self
            .app
            .query_row(
                "SELECT id FROM users WHERE role = 'admin' ORDER BY id LIMIT 1",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::Missing("admin user".into()))?;
        UserId::from_str(&raw).map_err(Into::into)
    }

    pub fn delete_user(&self, id: UserId) -> Result<bool, StoreError> {
        let admin_id = self.admin_user_id()?;
        if id == admin_id {
            return Err(StoreError::Protected("admin user".into()));
        }
        if self.get_user(id)?.is_none() {
            return Ok(false);
        }

        // SQLite 文件之间无法组成一个原子事务。先做可恢复的所有权转移：任一步失败时
        // 用户仍存在，重试即可；最后才删除不可恢复的成员私有状态和用户行。
        let subscribe_tx = self.subscribe.unchecked_transaction()?;
        subscribe_tx.execute(
            "UPDATE subscribes SET user_id = ?1 WHERE user_id = ?2",
            params![admin_id.to_string(), id.to_string()],
        )?;
        subscribe_tx.commit()?;

        let app_tx = self.app.unchecked_transaction()?;
        app_tx.execute(
            "UPDATE collections SET user_id = ?1 WHERE user_id = ?2",
            params![admin_id.to_string(), id.to_string()],
        )?;
        app_tx.commit()?;

        let playback_tx = self.subscribe.unchecked_transaction()?;
        for table in [
            "playback_units",
            "playback_sessions",
            "playback_logs",
            "playback_metrics",
        ] {
            playback_tx.execute(
                &format!("DELETE FROM {table} WHERE user_id = ?1"),
                params![id.to_string()],
            )?;
        }
        playback_tx.commit()?;

        let user_tx = self.app.unchecked_transaction()?;
        user_tx.execute(
            "DELETE FROM user_tokens WHERE user_id = ?1",
            params![id.to_string()],
        )?;
        let n = user_tx.execute("DELETE FROM users WHERE id = ?1", params![id.to_string()])?;
        user_tx.commit()?;
        Ok(n > 0)
    }

    /// 只解析未过期的会话 token（签发后 Session TTL 秒内有效；管理员配置的系统 CLI token 持续有效）。
    pub fn user_by_token(&self, token: &str) -> Result<Option<User>, StoreError> {
        let current_cli: Option<String> = self
            .app
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![SETTING_CLI_TOKEN],
                |row| row.get(0),
            )
            .optional()?;
        let is_cli = current_cli.as_deref() == Some(token);
        let min_created = now_unix() - SESSION_TTL_SECS;
        self.app
            .query_row(
                "SELECT u.id, u.login, u.enabled, COALESCE(u.role, 'member') FROM users u
                 JOIN user_tokens t ON t.user_id = u.id
                 WHERE t.token = ?1 AND (t.created_at >= ?2 OR (?3 = 1 AND u.role = 'admin')) AND u.enabled = 1",
                params![token, min_created, if is_cli { 1 } else { 0 }],
                map_user,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn user_id_by_token(&self, token: &str) -> Result<Option<UserId>, StoreError> {
        Ok(self.user_by_token(token)?.map(|user| user.id))
    }

    /// 获取用户的最新有效会话 token（用于播放/流式 URL 凭证补全）。
    pub fn latest_token_for_user(&self, user_id: UserId) -> Result<Option<String>, StoreError> {
        self.app
            .query_row(
                "SELECT token FROM user_tokens WHERE user_id = ?1 ORDER BY created_at DESC LIMIT 1",
                params![user_id.to_string()],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    /// 校验密码并签发新会话 token（凭据与会话分离：密码存 users.password，
    /// user_tokens 只存会话 token）。返回新签发的会话 token。
    /// 同一用户重复登录会轮换会话（旧会话 token 失效，表有界）。
    pub fn verify_user_password(
        &self,
        login: &str,
        password: &str,
    ) -> Result<Option<String>, StoreError> {
        if password.is_empty() {
            return Ok(None);
        }
        let user_info = self
            .app
            .query_row(
                "SELECT id, password FROM users WHERE login = ?1 AND enabled = 1",
                params![login],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
            )
            .optional()?;
        let Some((uid_str, stored_pw_opt)) = user_info else {
            return Ok(None);
        };
        let Some(stored_pw) = stored_pw_opt else {
            return Ok(None);
        };
        if !crate::password::verify_password(password, &stored_pw) {
            return Ok(None);
        }

        let new_hash = if !crate::password::is_argon2_hash(&stored_pw) {
            let hash = crate::password::hash_password(password).map_err(|e| {
                tracing::error!(user_id = %uid_str, error = %e, "存量明文口令哈希生成失败");
                StoreError::PasswordHash(e)
            })?;
            Some(hash)
        } else {
            None
        };

        self.finish_verified_login(&uid_str, &stored_pw, new_hash.as_deref())
    }

    /// 读取密码校验所需的不可变登录快照（用户 id + 当时观察到的 password 列）。
    /// 校验本身必须在 Store 锁之外执行；之后用 [`Store::finish_verified_login`]
    /// 以 CAS 语义完成签发。`password` 为 NULL 或用户不存在/被禁用时返回 `None`。
    ///
    /// 故意不 derive `Debug`/`Serialize`：快照携带口令材料，不得进入日志。
    pub fn login_credential(&self, login: &str) -> Result<Option<LoginCredential>, StoreError> {
        let row = self
            .app
            .query_row(
                "SELECT id, password FROM users WHERE login = ?1 AND enabled = 1",
                params![login],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
            )
            .optional()?;
        let Some((id, Some(observed_password))) = row else {
            return Ok(None);
        };
        // 空口令行（仅 `insert_user` 建立、从未设置密码）没有可校验的凭据。
        if observed_password.is_empty() {
            return Ok(None);
        }
        Ok(Some(LoginCredential {
            user_id: UserId::from_str(&id)?,
            observed_password,
        }))
    }

    /// 在同一个事务中完成 CAS 密码升级（若需要）并原子签发会话 token。
    /// 若处于升级路径且并发修改导致 CAS 未击中，返回 Ok(None) 并回滚，绝不签发过期凭据。
    pub fn finish_verified_login(
        &self,
        user_id: &str,
        observed_password: &str,
        new_hash: Option<&str>,
    ) -> Result<Option<String>, StoreError> {
        let tx = self.app.unchecked_transaction()?;
        if let Some(hash) = new_hash {
            let changed = tx.execute(
                "UPDATE users SET password = ?1 WHERE id = ?2 AND password = ?3 AND enabled = 1",
                params![hash, user_id, observed_password],
            )?;
            if changed == 0 {
                tracing::warn!(user_id = %user_id, "存量密码升级 CAS 冲突或用户已被禁用，放弃登录会话签发");
                return Ok(None);
            }
        } else {
            let active = tx
                .query_row(
                    "SELECT 1 FROM users WHERE id = ?1 AND password = ?2 AND enabled = 1",
                    params![user_id, observed_password],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
            if !active {
                tracing::warn!(user_id = %user_id, "用户状态或口令在校验期间发生变更，取消会话签发");
                return Ok(None);
            }
        }

        let token = Uuid::new_v4().simple().to_string();
        tx.execute(
            "INSERT INTO user_tokens (token, user_id, created_at)
             VALUES (?1, ?2, ?3)",
            params![token, user_id, now_unix()],
        )?;
        tx.commit()?;
        Ok(Some(token))
    }
}

fn apply_cli_token_tx(
    tx: &rusqlite::Transaction<'_>,
    admin_id: UserId,
    token: &str,
) -> Result<(), StoreError> {
    let existing_owner: Option<String> = tx
        .query_row(
            "SELECT user_id FROM user_tokens WHERE token = ?1",
            params![token],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(owner) = existing_owner {
        if owner != admin_id.to_string() {
            tracing::error!(%admin_id, "待登记的 CLI Token 与现有其他用户会话碰撞，拒绝提升权限");
            return Err(StoreError::CredentialConflict(
                "待配置的系统 CLI Token 与现有其他用户的会话凭据碰撞".into(),
            ));
        }
    }
    let old_token: Option<String> = tx
        .query_row(
            "SELECT value FROM settings WHERE key = ?1",
            params![SETTING_CLI_TOKEN],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(old) = old_token {
        if old != token {
            tx.execute(
                "DELETE FROM user_tokens WHERE user_id = ?1 AND token = ?2",
                params![admin_id.to_string(), old],
            )?;
        }
    } else {
        tx.execute(
            "DELETE FROM user_tokens WHERE user_id = ?1",
            params![admin_id.to_string()],
        )?;
    }
    tx.execute(
        "INSERT OR REPLACE INTO user_tokens (token, user_id, created_at) VALUES (?1, ?2, ?3)",
        params![token, admin_id.to_string(), now_unix()],
    )?;
    tx.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
        params![SETTING_CLI_TOKEN, token],
    )?;
    Ok(())
}
