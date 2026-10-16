//! G-M5: only one API instance may drive a data directory. The lease lives in
//! the app DB, is refreshed by a heartbeat thread and released on drop; a lease
//! whose heartbeat is older than the TTL can be taken over after a crash.
//!
//! Review L-7: the lease compares wall-clock times (`unix_now`) written by
//! different processes. A large NTP step forward can make a live holder look
//! stale (takeover; the old holder notices on its next heartbeat and turns
//! read-only), and a step backward can delay takeover of a crashed holder.
//! Keep the hosts' clocks synced; the TTL (60 s) is the tolerance window.

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
    /// H-1: set by the heartbeat when another instance took the lease over.
    lost: std::sync::atomic::AtomicBool,
}

impl InstanceLease {
    pub(crate) fn is_lost(&self) -> bool {
        self.lost.load(std::sync::atomic::Ordering::SeqCst)
    }
}

fn env_ms(name: &str, default: Duration) -> Duration {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(default)
}

/// H-1: how often a read-only instance retries the lease (TTL/2).
pub(crate) fn retry_interval() -> Duration {
    env_ms(
        "CRAWLER_MEDIA_LEASE_RETRY_MS",
        Duration::from_secs((INSTANCE_LEASE_TTL_SECS / 2) as u64),
    )
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
        lost: std::sync::atomic::AtomicBool::new(false),
    });
    spawn_heartbeat(Arc::downgrade(&lease));
    Ok(Some(lease))
}

fn spawn_heartbeat(lease: Weak<InstanceLease>) {
    let spawned = std::thread::Builder::new()
        .name("instance-lease".into())
        .spawn(move || {
            loop {
                std::thread::sleep(env_ms("CRAWLER_MEDIA_LEASE_HEARTBEAT_MS", HEARTBEAT_EVERY));
                let Some(lease) = lease.upgrade() else { return };
                let now = crate::job_loop::unix_now();
                match lease
                    .store
                    .lock()
                    .heartbeat_instance_lock(&lease.owner, now)
                {
                    Ok(true) => {}
                    Ok(false) => {
                        tracing::error!(owner = %lease.owner, "实例锁已被其他实例接管：停止后台任务并降级为只读");
                        lease.lost.store(true, std::sync::atomic::Ordering::SeqCst);
                        return;
                    }
                    Err(error) => tracing::warn!(%error, "实例锁心跳失败"),
                }
            }
        });
    if let Err(error) = spawned {
        tracing::error!(%error, "无法启动实例锁心跳线程");
    }
}
