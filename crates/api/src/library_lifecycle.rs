//! Library-scoped work leases. Registry -> Store is the only permitted lock order.
use crate::management::ApiState;

impl ApiState {
    pub(crate) fn acquire_path_work(&self, path: &std::path::Path, kind: domain::MediaKind)
        -> Result<Option<LibraryWorkLease>, String>
    {
        let owner = {
            let store = self.store.lock();
            let owner = store.library_for_path_strict(path, kind).map_err(|e| e.to_string())?;
            if owner.is_none() && store.path_in_removal_scope(path).map_err(|e| e.to_string())? {
                return Err("Library 已删除，拒绝旧目录工作".into());
            }
            owner
        };
        owner.map(|owner| self.acquire_library_work(&owner.id)).transpose()
    }
    pub fn acquire_library_work(&self, library_id: &str) -> Result<LibraryWorkLease, String> {
        self.library_work.acquire(&self.store, library_id)
    }

    /// 等该库当前在跑的后台工作全部结束（建库自动扫描、显式扫描、重导入共用同一
    /// registry；由 lease drop 的 `notify_waiters` 唤醒，不是定时轮询）。
    ///
    /// 调用方负责加超时。存在的理由：建库会自动调度首次扫描，与显式 `POST /scan`
    /// 并发，先返回的那次可能还没做完身份重绑，观察者需要一个确定性同步点。
    pub async fn wait_library_idle(&self, library_id: &str) {
        self.library_work.wait_idle(library_id).await;
    }
}

pub fn start_library_removal(state: &ApiState, library_id: &str)
    -> Result<store::LibraryRemoval, String>
{
    let handle = tokio::runtime::Handle::try_current().map_err(|e| e.to_string())?;
    let operation = state.library_work.freeze(&state.store, library_id).map_err(|e| e.to_string())?;
    launch_library_removal(state, operation, handle)
}

pub(crate) fn launch_library_removal(state: &ApiState, mut operation: store::LibraryRemoval,
    handle: tokio::runtime::Handle) -> Result<store::LibraryRemoval, String> {
    if operation.state == "succeeded" { return Ok(operation); }
    if !state.library_work.removals.lock().insert(operation.operation_id.clone()) {
        return Ok(operation);
    }
    if operation.state == "failed" {
        if let Err(error) = state.store.lock().set_removal_state(&operation.operation_id, "draining", None) {
            state.library_work.removals.lock().remove(&operation.operation_id);
            return Err(error.to_string());
        }
        operation.state = "draining".into();
        operation.error = None;
    }
    let state = state.clone();
    let operation_id = operation.operation_id.clone();
    handle.spawn(async move {
        if let Err(error) = drain_removal(&state, &operation_id).await {
            tracing::error!(%operation_id, %error, "Library 删除失败，保持冻结");
            if let Err(persist_error) = state.store.lock().set_removal_state(&operation_id, "failed", Some(&error)) {
                tracing::error!(%operation_id, %persist_error, "保存 Library 删除失败状态失败");
            }
        }
        state.library_work.removals.lock().remove(&operation_id);
    });
    Ok(operation)
}

async fn drain_removal(state: &ApiState, operation_id: &str) -> Result<(), String> {
    let (library_id, ledger_ids) = {
        let store = state.store.lock();
        let operation = store.library_removal(operation_id).map_err(|e| e.to_string())?
            .ok_or("Library removal missing")?;
        if operation.state == "succeeded" { return Ok(()); }
        store.set_removal_state(operation_id, "draining", None).map_err(|e| e.to_string())?;
        (operation.library_id, store.removal_ledger_ids(operation_id).map_err(|e| e.to_string())?)
    };
    tracing::info!(%operation_id, %library_id, total_ledgers = ledger_ids.len(), "开始排空 Library 关联任务与活跃会话");
    state.library_work.cancel(&library_id);
    state.probe.cancel_ledger_work(&ledger_ids, "Library removed").map_err(|e| e.to_string())?;

    {
        let store = state.store.lock();
        store
            .close_playback_sessions_for_removal(operation_id)
            .map_err(|e| format!("关闭待删除媒体库播放会话失败: {e}"))?;
    }

    if let Err(_) = tokio::time::timeout(std::time::Duration::from_secs(30), state.library_work.wait_idle(&library_id)).await {
        tracing::error!(%operation_id, %library_id, "等待 Library 工作租约释放超时 (30s)，删除操作失败并保持冻结");
        return Err("Library work did not stop within 30 seconds".to_string());
    }
    tracing::info!(%operation_id, %library_id, "Library 任务已全部排空，进入清理阶段");
    let covers_dir = {
        let store = state.store.lock();
        store.set_removal_state(operation_id, "cleaning", None).map_err(|e| e.to_string())?;
        store.clean_library_removal(operation_id).map_err(|e| e.to_string())?;
        store.library_covers_dir()
    };
    // 封面文件无法进 DB 事务：此时 Library 已冻结且行已删除，没有并发发布能重建它；
    // 启动时的对账（purge_removed_library_covers）负责补做崩溃窗口内漏掉的那次删除。
    crate::library_cover_service::remove_library_cover_files(&covers_dir, &library_id);
    state.projection.remove_library(&library_id);
    tracing::info!(%operation_id, %library_id, "Library 索引清理完成，删除操作成功");
    Ok(())
}

pub fn resume_library_removals(state: &ApiState) -> Result<(), String> {
    let operations = state.store.lock().pending_library_removals().map_err(|e| e.to_string())?;
    for operation in operations {
        state.library_work.cancel(&operation.library_id);
        if operation.state == "failed" {
            tracing::warn!(operation_id = %operation.operation_id, library_id = %operation.library_id,
                "Library 删除此前已失败，重启后保持冻结状态，等待手动重试");
            continue;
        }
        start_library_removal(state, &operation.library_id)?;
    }
    Ok(())
}
use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::Notify;

#[derive(Default)]
pub struct LibraryWorkRegistry {
    entries: Mutex<HashMap<String, Entry>>,
    changed: Notify,
    removals: Mutex<std::collections::HashSet<String>>,
}

#[derive(Default)]
struct Entry {
    cancelled: bool,
    controls: HashMap<String, library::WorkControl>,
}

pub struct LibraryWorkLease {
    registry: Arc<LibraryWorkRegistry>,
    library_id: String,
    lease_id: String,
    control: library::WorkControl,
}

impl LibraryWorkLease {
    pub fn library_id(&self) -> &str { &self.library_id }
    pub fn publish<T>(&self, publish: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
        let entries = self.registry.entries.lock();
        if entries.get(&self.library_id).is_none_or(|entry| entry.cancelled) {
            return Err("Library work cancelled".into());
        }
        self.control.publish(publish)
    }
    pub fn control(&self) -> &library::WorkControl { &self.control }
}

impl Drop for LibraryWorkLease {
    fn drop(&mut self) {
        if let Some(entry) = self.registry.entries.lock().get_mut(&self.library_id) {
            entry.controls.remove(&self.lease_id);
        }
        self.registry.changed.notify_waiters();
    }
}

impl LibraryWorkRegistry {
    pub fn acquire(self: &Arc<Self>, store: &Mutex<store::Store>, library_id: &str)
        -> Result<LibraryWorkLease, String>
    {
        let mut entries = self.entries.lock();
        let entry = entries.entry(library_id.to_string()).or_default();
        if entry.cancelled || !store.lock().library_accepts_work(library_id).map_err(|e| e.to_string())? {
            return Err("Library 正在删除或已不存在".into());
        }
        let lease_id = uuid::Uuid::new_v4().to_string();
        let control = library::WorkControl::default();
        entry.controls.insert(lease_id.clone(), control.clone());
        Ok(LibraryWorkLease { registry: self.clone(), library_id: library_id.into(), lease_id, control })
    }

    pub fn freeze(&self, store: &Mutex<store::Store>, library_id: &str)
        -> Result<store::LibraryRemoval, store::StoreError>
    {
        let mut entries = self.entries.lock();
        let operation = store.lock().begin_library_removal(library_id)?;
        let entry = entries.entry(library_id.into()).or_default();
        entry.cancelled = true;
        for control in entry.controls.values() { control.cancel(); }
        Ok(operation)
    }

    pub fn active_control(&self, library_id: &str) -> Option<library::WorkControl> {
        self.entries.lock().get(library_id).and_then(|entry| entry.controls.values().next().cloned())
    }

    /// 是否有任何 Library 工作正在进行（维护任务据此跳过重型操作）。
    ///
    /// 这只是**降低冲突**：返回 false 不代表之后不会被新扫描抢到锁，SQLite
    /// busy 处理与有界执行仍然必须存在。
    pub fn has_active_work(&self) -> bool {
        self.entries
            .lock()
            .values()
            .any(|entry| !entry.controls.is_empty())
    }

    pub fn cancel(&self, library_id: &str) {
        let mut entries = self.entries.lock();
        let entry = entries.entry(library_id.into()).or_default();
        entry.cancelled = true;
        for control in entry.controls.values() { control.cancel(); }
    }

    pub async fn wait_idle(&self, library_id: &str) {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let idle = self.entries.lock().get(library_id).is_none_or(|entry| entry.controls.is_empty());
            if idle { return; }
            notified.await;
        }
    }
}
