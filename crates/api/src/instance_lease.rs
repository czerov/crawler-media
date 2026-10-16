//! G-M5: only one API instance may drive a data directory. The lease lives in
//! the app DB, is refreshed by a heartbeat thread and released on drop; a lease
//! whose heartbeat is older than the TTL can be taken over after a crash.

use std::sync::{Arc, Weak};
use std::time::Duration;

use parking_lot::Mutex;

use crate::store::Store;

/// Lease TTL: a crashed instance's lock is taken over after this many seconds.
pub const INSTANCE_LEASE_TTL_SECS: i64 = 60;
const HEARTBEAT_EVERY: Duration = Duration::from_secs(20);
/// Escape hatch for deliberate multi-instance setups (env var, "1"/"true").
pub const ALLOW_MULTI_INSTANCE_ENV: &str = "CRAWLER_MEDIA_ALLOW_MULTI_INSTANCE";

pub struct InstanceLease {
    store: Arc<Mutex<Store>>,
    owner: String,
}

impl Drop for InstanceLease {
    fn drop(&mut self) {
        if let Err(error) = self.store.lock().release_instance_lock(&self.owner) {
            tracing::warn!(%error, "释放实例锁失败");
        }
    }
}

fn multi_instance_allowed(store: &Store) -> bool {
    let truthy = |v: &str| matches!(v.trim(), "1" | "true" | "yes");
    std::env::var(ALLOW_MULTI_INSTANCE_ENV).is_ok_and(|v| truthy(&v))
        || store
            .get_setting(crate::settings_keys::INSTANCE_ALLOW_MULTI)
            .ok()
            .flatten()
            .is_some_and(|v| truthy(&v))
}

/// Acquire the data-directory lease; `Err` when another live instance holds it.
pub(crate) fn acquire(store: &Arc<Mutex<Store>>) -> std::io::Result<Option<Arc<InstanceLease>>> {
    let owner = format!(
        "pid{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    );
    {
        let guard = store.lock();
        if multi_instance_allowed(&guard) {
            tracing::warn!("已允许多实例共用数据目录：订阅选种/入库可能重复执行");
            return Ok(None);
        }
        let now = crate::job_loop::unix_now();
        match guard.try_acquire_instance_lock(&owner, now, INSTANCE_LEASE_TTL_SECS) {
            Ok(None) => {}
            Ok(Some(other)) => {
                return Err(std::io::Error::other(format!(
                    "数据目录已被另一个实例 ({other}) 占用；仅支持单实例运行（设置 {ALLOW_MULTI_INSTANCE_ENV}=1 可跳过）"
                )));
            }
            Err(error) => return Err(std::io::Error::other(error.to_string())),
        }
    }
    tracing::info!(%owner, "已获取数据目录实例锁（仅支持单实例运行）");
    let lease = Arc::new(InstanceLease {
        store: store.clone(),
        owner,
    });
    spawn_heartbeat(Arc::downgrade(&lease));
    Ok(Some(lease))
}

fn spawn_heartbeat(lease: Weak<InstanceLease>) {
    let spawned = std::thread::Builder::new()
        .name("instance-lease".into())
        .spawn(move || {
            loop {
                std::thread::sleep(HEARTBEAT_EVERY);
                let Some(lease) = lease.upgrade() else { return };
                let now = crate::job_loop::unix_now();
                match lease
                    .store
                    .lock()
                    .heartbeat_instance_lock(&lease.owner, now)
                {
                    Ok(true) => {}
                    Ok(false) => tracing::error!(owner = %lease.owner, "实例锁已被其他实例接管"),
                    Err(error) => tracing::warn!(%error, "实例锁心跳失败"),
                }
            }
        });
    if let Err(error) = spawned {
        tracing::error!(%error, "无法启动实例锁心跳线程");
    }
}
