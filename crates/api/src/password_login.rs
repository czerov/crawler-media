//! 两套登录协议（REST `/api/v1/auth/login` 与 Jellyfin `/Users/AuthenticateByName`）
//! 共用的密码校验与 CAS 签发服务。
//!
//! Argon2 校验必须离开 Store 锁：闭包先在锁内取一次不可变快照，然后在
//! `spawn_blocking` 中做 CPU 密集的 verify/hash，最后只用一次短锁完成
//! `finish_verified_login` 的 CAS 与 token→user 查询。并发 permit 由闭包
//! 持有，直到 CAS 结束才释放，避免校验排队期间无限堆积阻塞线程。

use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::Store;

/// 可注入的密码原语；测试用它阻塞或观测校验过程。
pub trait PasswordEngine: Send + Sync {
    fn verify(&self, password: &str, stored: &str) -> bool;
    fn hash(&self, password: &str) -> Result<String, String>;
}

/// 生产实现：复用 store 的 Argon2id 参数与明文透明升级规则。
pub struct Argon2Engine;

impl PasswordEngine for Argon2Engine {
    fn verify(&self, password: &str, stored: &str) -> bool {
        store::password::verify_password(password, stored)
    }

    fn hash(&self, password: &str) -> Result<String, String> {
        store::password::hash_password(password)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LoginError {
    #[error("password service busy")]
    Busy,
    #[error(transparent)]
    Store(#[from] store::StoreError),
    #[error("password worker failed: {0}")]
    Worker(String),
    #[error("password hashing failed: {0}")]
    Hash(String),
}

pub struct PasswordLogin {
    engine: Arc<dyn PasswordEngine>,
    permits: Arc<Semaphore>,
}

impl PasswordLogin {
    pub fn new(engine: Arc<dyn PasswordEngine>, concurrency: usize) -> Self {
        Self {
            engine,
            permits: Arc::new(Semaphore::new(concurrency.max(1))),
        }
    }

    /// 校验口令并在 CAS 命中时签发新会话 token。
    ///
    /// 返回 `Ok(None)` 表示凭据无效、用户被禁用/删除或快照在校验期间失效，
    /// 调用方必须映射为 401 且不签发任何 token。`Err(Busy)` 表示并发已满。
    pub async fn authenticate(
        &self,
        store: Arc<Mutex<Store>>,
        username: String,
        password: String,
    ) -> Result<Option<(String, domain::User)>, LoginError> {
        if password.is_empty() {
            return Ok(None);
        }
        let permit = self
            .permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| LoginError::Busy)?;
        let credential = store.lock().login_credential(&username)?;
        let Some(credential) = credential else {
            return Ok(None);
        };
        let engine = self.engine.clone();
        tokio::task::spawn_blocking(move || {
            // permit 必须活到 CAS 结束：闭包返回前不得让出并发槽位。
            let _permit: OwnedSemaphorePermit = permit;
            if !engine.verify(&password, &credential.observed_password) {
                return Ok(None);
            }
            let upgrade = if store::password::is_argon2_hash(&credential.observed_password) {
                None
            } else {
                Some(engine.hash(&password).map_err(LoginError::Hash)?)
            };
            let store = store.lock();
            let token = store.finish_verified_login(
                &credential.user_id.to_string(),
                &credential.observed_password,
                upgrade.as_deref(),
            )?;
            match token {
                None => Ok(None),
                Some(token) => {
                    let user = store.user_by_token(&token)?.ok_or_else(|| {
                        LoginError::Worker("issued token has no active user".into())
                    })?;
                    Ok(Some((token, user)))
                }
            }
        })
        .await
        .map_err(|error| LoginError::Worker(error.to_string()))?
    }
}
