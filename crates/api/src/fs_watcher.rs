use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use notify::RecursiveMode;
use notify_debouncer_mini::{DebouncedEvent, new_debouncer};
use parking_lot::Mutex;

use crate::management::ApiState;

pub(crate) mod presence;
/// STRM 延迟删除条目：记录删除时间戳与删除前的流媒体 URL。
#[derive(Clone, Debug)]
pub struct PendingStrmDeletion {
    pub deleted_at: i64,
    pub previous_url: Option<String>,
}

/// 全局 STRM 延时防抖状态追踪器
#[derive(Default)]
pub struct StrmGraceTracker {
    /// path -> PendingStrmDeletion
    pending: Arc<Mutex<HashMap<PathBuf, PendingStrmDeletion>>>,
    known_urls: Arc<Mutex<HashMap<PathBuf, String>>>,
    /// 测试注入的当前时间。0 表示使用系统时间。
    now_override: AtomicI64,
}

impl StrmGraceTracker {
    pub fn new() -> Self {
        Self {
            pending: Arc::new(Mutex::new(HashMap::new())),
            known_urls: Arc::new(Mutex::new(HashMap::new())),
            now_override: AtomicI64::new(0),
        }
    }

    /// 测试用：固定宽限期计算使用的当前时间。
    pub fn set_now(&self, now: i64) {
        self.now_override.store(now, Ordering::Relaxed);
    }

    pub fn current_now(&self) -> i64 {
        match self.now_override.load(Ordering::Relaxed) {
            0 => crate::job_loop::unix_now(),
            now => now,
        }
    }

    /// 标记一个 .strm 文件被删除，进入延时观察期（默认 45 秒）
    pub fn mark_deleted(&self, path: PathBuf, previous_url: Option<String>, now: i64) {
        let previous_url = previous_url.or_else(|| self.known_urls.lock().remove(&path));
        let mut map = self.pending.lock();
        tracing::info!(
            path = %path.display(),
            "STRM 文件已删除，进入宽限期（45 秒）"
        );
        map.insert(
            path,
            PendingStrmDeletion {
                deleted_at: now,
                previous_url,
            },
        );
    }

    /// 检查并尝试对冲重建：
    /// 如果在观察期内重新出现，比对 URL 内容；
    /// 若内容不变，取消删除并返回 false（表示无需刷新元数据/封面）；
    /// 若是全新文件或 URL 发生变化，返回 true。
    pub fn on_created_or_modified(&self, path: &Path, current_url: Option<&str>) -> bool {
        let prev_url = if let Some(url) = current_url {
            self.known_urls
                .lock()
                .insert(path.to_path_buf(), url.to_string())
        } else {
            None
        };
        let mut map = self.pending.lock();
        if let Some(pending) = map.remove(path) {
            let same_content = match (&pending.previous_url, current_url) {
                (Some(old_u), Some(new_u)) => old_u.trim() == new_u.trim(),
                _ => false,
            };
            if same_content {
                tracing::info!(
                    path = %path.display(),
                    "STRM 文件在宽限期内以相同 URL 重建，取消删除并跳过重新刮削"
                );
                return false;
            } else {
                tracing::info!(
                    path = %path.display(),
                    "STRM 文件以新 URL 重建，保留并允许刷新"
                );
                return true;
            }
        }
        // 如果文件一直在磁盘上且 URL 未曾变化（如仅更新文件时间戳或写同 URL），不应触发标记失效与重刮削
        if let (Some(prev), Some(curr)) = (prev_url, current_url) {
            if prev.trim() == curr.trim() {
                return false;
            }
        }
        true
    }

    /// 清理已超时的待删除条目（超时仍未重建，执行真实台账删除）
    pub fn forget(&self, path: &Path) {
        self.known_urls.lock().remove(path);
    }

    pub fn expire_ready(&self, grace_secs: i64) -> Vec<PathBuf> {
        self.sweep_expired(self.current_now(), grace_secs)
    }

    pub fn sweep_expired(&self, now: i64, grace_secs: i64) -> Vec<PathBuf> {
        let mut map = self.pending.lock();
        let mut expired = Vec::new();
        map.retain(|path, item| {
            if now - item.deleted_at >= grace_secs {
                // 超时后确认磁盘上是否真不存在
                if !path.exists() {
                    expired.push(path.clone());
                }
                false
            } else {
                true
            }
        });
        expired
    }
}

/// 启动全局文件系统事件监控
pub fn spawn_fs_watcher(state: ApiState) {
    tokio::spawn(async move {
        let tracker = Arc::new(StrmGraceTracker::new());
        if let Ok(rows) = state.store.lock().list_ledger() {
            for row in rows {
                let path = Path::new(&row.path);
                if path
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("strm"))
                {
                    if let Some(url) = library::read_strm_url(path) {
                        tracker.on_created_or_modified(path, Some(&url));
                    }
                }
            }
        }

        // 启动后台定时任务，定期维护 STRM 内存防抖追踪
        let sweep_tracker = tracker.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(5)).await;
                for path in sweep_tracker.expire_ready(45) {
                    tracing::info!(path = %path.display(), "STRM 宽限期已过，清除内存防抖");
                    sweep_tracker.forget(&path);
                }
            }
        });

        // 通道接收 notify 防抖事件
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<DebouncedEvent>>(100);
        let debouncer_tx = tx.clone();

        let mut debouncer = match new_debouncer(
            Duration::from_secs(3),
            move |res: Result<Vec<DebouncedEvent>, _>| {
                if let Ok(events) = res {
                    let _ = debouncer_tx.blocking_send(events);
                }
            },
        ) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(error = %e, "初始化 notify 防抖监听失败");
                return;
            }
        };

        let mut watcher_state = FsWatcherState::new();
        match current_dirs_to_watch(&state) {
            Ok(desired) => {
                tracing::info!(
                    dirs = ?desired.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
                    "fs_watcher 初始化监听目录列表"
                );
                watcher_state.reconcile(&mut debouncer, &desired);
            }
            Err(error) => {
                tracing::error!(%error, "初始化获取监听目录配置失败");
            }
        }

        let mut refresh_interval = tokio::time::interval(Duration::from_secs(10));

        loop {
            tokio::select! {
                _ = refresh_interval.tick() => {
                    match current_dirs_to_watch(&state) {
                        Ok(desired) => {
                            watcher_state.reconcile(&mut debouncer, &desired);
                        }
                        Err(error) => {
                            tracing::warn!(%error, "刷新监听目录配置失败，保留当前已有监听状态并待重试");
                        }
                    }
                }
                Some(events) = rx.recv() => {
                    let events = relevant_fs_events(&events);
                    if events.is_empty() {
                        continue;
                    }
                    tracing::info!(
                        event_count = events.len(),
                        paths = ?events.iter().map(|e| e.path.display().to_string()).collect::<Vec<_>>(),
                        "fs_watcher 接收到底层文件系统事件"
                    );
                    handle_fs_events(&state, &tracker, events);
                }
            }
        }
    });
}

fn current_dirs_to_watch(state: &ApiState) -> Result<HashSet<PathBuf>, store::StoreError> {
    let store = state.store.lock();
    let mut dirs = HashSet::new();
    if let Some(intake) = store.watch_intake()? {
        if !intake.is_empty() {
            dirs.insert(PathBuf::from(intake));
        }
    }
    if let Some(inplace) = store.watch_inplace()? {
        if !inplace.is_empty() {
            dirs.insert(PathBuf::from(inplace));
        }
    }
    let libraries = store.list_libraries()?;
    for lib in libraries {
        if lib.realtime_watch {
            for root in lib.root_paths {
                dirs.insert(root);
            }
        }
    }
    Ok(dirs)
}

fn is_path_in_watch_intake(state: &ApiState, path: &Path) -> bool {
    let Ok(Some(intake)) = state.store.lock().watch_intake() else {
        return false;
    };
    if intake.is_empty() {
        return false;
    }
    let intake_buf = PathBuf::from(&intake);
    let resolved_intake = std::fs::canonicalize(&intake_buf).unwrap_or(intake_buf);
    let resolved_path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    resolved_path.starts_with(&resolved_intake) || path.starts_with(&intake)
}

pub(crate) fn realtime_library_root_for_path(state: &ApiState, path: &Path) -> Option<PathBuf> {
    let resolved_path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    state
        .store
        .lock()
        .list_libraries()
        .ok()?
        .into_iter()
        .filter(|library| library.realtime_watch)
        .flat_map(|library| library.root_paths)
        .filter(|root| {
            let resolved_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.clone());
            resolved_path.starts_with(&resolved_root) || path.starts_with(root)
        })
        .max_by_key(|root| root.components().count())
}

/// 计算变动文件在媒体库中的条目/扫描目标目录。
///
/// 若文件位于媒体库根目录直属子目录下（如 `<root>/Show Title/Season 1/ep.strm`
/// 或 `<root>/Movie Title/movie.strm`），返回条目根目录 `<root>/Show Title`；
/// 若直接平铺在 `<root>` 下，则返回 `<root>` 本身。
pub(crate) fn entry_scan_target_dir(root: &Path, file_path: &Path) -> PathBuf {
    let resolved_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let resolved_path = std::fs::canonicalize(file_path).unwrap_or_else(|_| file_path.to_path_buf());
    let rel_opt = resolved_path
        .strip_prefix(&resolved_root)
        .ok()
        .or_else(|| file_path.strip_prefix(root).ok());
    if let Some(rel) = rel_opt {
        let mut components = rel.components();
        if let Some(first) = components.next() {
            let first_path = root.join(first.as_os_str());
            // 如果只有一级且是文件（如 root/movie.strm），则目标为 root；
            // 如果有多级（如 root/Show/Season 1/ep.strm 或 root/Movie/movie.strm），目标为 root/Show 或 root/Movie。
            if components.next().is_some() || first_path.is_dir() {
                return first_path;
            }
        }
    }
    root.to_path_buf()
}

pub fn relevant_fs_events(events: &[DebouncedEvent]) -> Vec<DebouncedEvent> {
    events
        .iter()
        .filter(|event| !is_ignored_filesystem_metadata(&event.path))
        .cloned()
        .collect()
}

fn is_ignored_filesystem_metadata(path: &Path) -> bool {
    path.components().any(|component| {
        let name = component.as_os_str().to_string_lossy();
        matches!(
            name.as_ref(),
            ".DS_Store"
                | "._.DS_Store"
                | ".AppleDouble"
                | ".AppleDB"
                | ".Spotlight-V100"
                | ".Trashes"
                | ".TemporaryItems"
                | ".fseventsd"
                | "#recycle"
                | "#Recycle"
                | "@Recycle"
                | "@recycle"
                | "$RECYCLE.BIN"
                | "Recycle.Bin"
                | "recycle_bin"
                | ".recycle"
        ) || name.starts_with("._")
            || name.eq_ignore_ascii_case("thumbs.db")
            || name.eq_ignore_ascii_case("desktop.ini")
    })
}

/// 检查文件系统变动路径是否为媒体库关注的目标（视频文件、STRM 或目录）。
/// 仅允许视频/STRM格式或目录变动向下传递，彻底屏蔽图片、NFO、字幕、文本、临时文件等非媒体侧车变动，
/// 防止刮削与元数据写入引发死循环，并统一使用完整的视频格式白名单。
pub(crate) fn is_relevant_media_entry(path: &Path) -> bool {
    if path.exists() {
        path.is_dir() || library::is_video_file(path)
    } else {
        // 对于已删除条目：无扩展名可能是目录；若有扩展名则必须是受支持的视频/STRM格式
        path.extension().is_none() || library::is_video_file(path)
    }
}

pub fn handle_fs_events(
    state: &ApiState,
    tracker: &Arc<StrmGraceTracker>,
    events: Vec<DebouncedEvent>,
) {
    let now = tracker.current_now();
    let mut has_intake_or_download_change = false;
    let mut has_scrape_needed = false;
    let mut library_scan_targets = HashSet::new();

    tracing::debug!(event_count = events.len(), "fs_watcher 收到文件系统变动事件批次");

    for event in events {
        let path = event.path;
        if !is_relevant_media_entry(&path) {
            continue;
        }
        tracing::debug!(path = %path.display(), exists = path.exists(), "处理文件系统事件路径");
        let is_strm = path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.eq_ignore_ascii_case("strm"))
            .unwrap_or(false);

        if !path.exists() {
            note_removed_path(state, tracker, &path, is_strm, now);
        } else {
            note_present_path(
                state,
                tracker,
                &path,
                is_strm,
                &mut has_intake_or_download_change,
                &mut has_scrape_needed,
                &mut library_scan_targets,
            );
        }
    }

    for path in tracker.expire_ready(45) {
        tracing::info!(path = %path.display(), "STRM 宽限期已过，结束内存防抖");
        tracker.forget(&path);
    }

    // 新增 STRM 在 Library 目录中需要执行 in-place scan 来写入台账；Scrape
    // 只负责侧车/元数据，不能替代 Library ledger ingestion。
    // 按变动的具体剧集/条目子目录进行增量扫描，避免遍历整个媒体库根目录。
    for (root, target_dir) in library_scan_targets {
        tracing::info!(
            root = %root.display(),
            target_dir = %target_dir.display(),
            "媒体库内 STRM 变动，触发增量子目录扫描入账"
        );
        let state = state.clone();
        tokio::task::spawn_blocking(move || {
            let result = crate::http::library_scan::scan_library_subdir(&state, &root, &target_dir);
            if let Err(error) = result {
                tracing::error!(
                    root = %root.display(),
                    target_dir = %target_dir.display(),
                    %error,
                    "媒体库 STRM 变动增量扫描入账失败"
                );
            }
        });
    }

    // 目录有新入库/下载文件完成，主动提前调度任务，不必等待 30 秒
    if has_intake_or_download_change {
        tracing::info!("文件系统事件触发即时转移与入站入库");
        let queue = state.jobs.lock();
        if let Ok(defs) = queue.list_defs() {
            for def in defs {
                if def.kind == jobs::JobKind::Transfer || def.kind == jobs::JobKind::WatchIntake {
                    let _ = queue.ensure_scheduled_for(0, def.id);
                }
            }
        }
    }

    if has_scrape_needed {
        tracing::info!("文件系统事件触发即时元数据刮削");
        let queue = state.jobs.lock();
        if let Ok(defs) = queue.list_defs() {
            for def in defs {
                if def.kind == jobs::JobKind::Scrape {
                    let _ = queue.ensure_scheduled_for(0, def.id);
                }
            }
        }
    }
}

fn note_removed_path(
    state: &ApiState,
    tracker: &StrmGraceTracker,
    path: &Path,
    is_strm: bool,
    now: i64,
) {
    tracing::info!(path = %path.display(), "检测到文件或目录缺失");
    if is_strm {
        let previous_url = tracker.known_urls.lock().get(path).cloned();
        tracker.mark_deleted(path.to_path_buf(), previous_url, now);
    }
    if let Err(e) = presence::observe_event(state, path) {
        tracing::warn!(path = %path.display(), error = ?e, "Watcher 缺失标记失败");
    }
}

fn note_present_path(
    state: &ApiState,
    tracker: &StrmGraceTracker,
    path: &Path,
    is_strm: bool,
    intake_changed: &mut bool,
    scrape_needed: &mut bool,
    scan_targets: &mut HashSet<(PathBuf, PathBuf)>,
) {
    tracing::info!(path = %path.display(), is_dir = path.is_dir(), is_strm, "检测到文件或目录新建或修改");
    note_present_path_presence(state, path);
    if is_path_in_watch_intake(state, path) {
        *intake_changed = true;
    }
    if path.is_dir() {
        if let Some(root) = realtime_library_root_for_path(state, path) {
            let target_dir = entry_scan_target_dir(&root, path);
            tracing::info!(root = %root.display(), target_dir = %target_dir.display(), "目录新增/还原，加入扫描目标");
            scan_targets.insert((root, target_dir));
        }
        return;
    }
    if is_strm {
        let current_url = library::read_strm_url(path);
        if tracker.on_created_or_modified(path, current_url.as_deref()) {
            invalidate_strm_caches(state, path);
            *scrape_needed = true;
            if let Some(root) = realtime_library_root_for_path(state, path) {
                let target_dir = entry_scan_target_dir(&root, path);
                tracing::info!(root = %root.display(), target_dir = %target_dir.display(), "STRM 新增/修改，加入扫描目标");
                scan_targets.insert((root, target_dir));
            }
        }
        return;
    }
    if library::is_video_file(path) {
        *intake_changed = true;
        if let Some(root) = realtime_library_root_for_path(state, path) {
            let target_dir = entry_scan_target_dir(&root, path);
            scan_targets.insert((root, target_dir));
        }
    }
}

fn note_present_path_presence(state: &ApiState, path: &Path) {
    if let Err(e) = presence::observe_event(state, path) {
        tracing::warn!(path = %path.display(), error = ?e, "Watcher 恢复标记失败");
    }
}

/// 事件路径不存在时，只删除同样已经不在磁盘上的台账行。
/// 父目录事件在处理瞬间可能 `exists()==false`，但其下仍在的 strm 不能被级联删掉。
fn ledger_path_is_gone(event_path: &Path, ledger_path: &str) -> bool {
    let row_path = Path::new(ledger_path);
    let under_event = ledger_path == event_path.display().to_string()
        || row_path.starts_with(event_path);
    under_event && !row_path.exists()
}

/// strm 文件内容（远程 URL）变化时，清掉旧 URL 的派生缓存：
/// - file_meta（视频/音轨/字幕流信息，探测缓存）
/// - cached chapters（内嵌章节缓存）
/// - media_markers（片头片尾标记）
/// 这些缓存按 ledger 行挂载，清理后下次访问详情页/播放时会按新 URL 重新探测。
fn invalidate_strm_caches(state: &ApiState, path: &std::path::Path) {
    let path_str = path.display().to_string();
    let store = state.store.lock();
    let Ok(Some(row)) = store.ledger_by_path(&path_str) else {
        return;
    };
    let ledger_id = row.id.to_string();
    let mut cleared = 0;
    if store.delete_file_meta_by_ledger_id(&ledger_id).is_ok() {
        cleared += 1;
    }
    if store.clear_cached_chapters(&ledger_id).is_ok() {
        cleared += 1;
    }
    // 只清当前行对应季集未锁定的标记：电视剧各集共用 media_id，绝不能删整部剧；用户手动锁定标记保留。
    let is_locked = store
        .get_media_marker(row.media_id, row.season, row.episode)
        .ok()
        .flatten()
        .map(|m| m.locked)
        .unwrap_or(false);
    if !is_locked
        && store
            .delete_media_marker(row.media_id, row.season, row.episode)
            .is_ok()
    {
        cleared += 1;
    }
    tracing::info!(
        path = %path_str,
        season = row.season,
        episode = row.episode,
        cleared,
        "【STRM】检测到 strm 内容变化，已清空该集旧探测缓存（流信息/章节/片头片尾），下次访问将按新 URL 重新探测"
    );
}

/// 抽象文件系统 Watcher 行为，方便单元测试注入 fake/mock
pub(crate) trait FsWatcherSession {
    fn watch_dir(&mut self, dir: &Path) -> Result<(), notify::Error>;
    fn unwatch_dir(&mut self, dir: &Path) -> Result<(), notify::Error>;
}

impl FsWatcherSession for notify_debouncer_mini::Debouncer<notify::RecommendedWatcher> {
    fn watch_dir(&mut self, dir: &Path) -> Result<(), notify::Error> {
        self.watcher().watch(dir, RecursiveMode::Recursive)
    }

    fn unwatch_dir(&mut self, dir: &Path) -> Result<(), notify::Error> {
        self.watcher().unwatch(dir)
    }
}

/// 维护 watcher 的 desired 与 active 集合状态
#[derive(Default, Debug)]
pub(crate) struct FsWatcherState {
    active: HashSet<PathBuf>,
}

impl FsWatcherState {
    pub(crate) fn new() -> Self {
        Self {
            active: HashSet::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn active(&self) -> &HashSet<PathBuf> {
        &self.active
    }

    /// 执行一轮 desired 状态收敛对齐。
    /// - desired: 期望监听的目录集合
    /// - watch 失败或目录不存在时不加入 active，保留下次刷新重试
    /// - unwatch 失败保留在 active，下次刷新重试
    pub(crate) fn reconcile<W: FsWatcherSession>(&mut self, watcher: &mut W, desired: &HashSet<PathBuf>) {
        // 新增监听：desired - active
        let to_add: Vec<PathBuf> = desired.difference(&self.active).cloned().collect();
        for dir in to_add {
            if !dir.is_dir() {
                tracing::warn!(dir = %dir.display(), "目标路径不是目录或尚不存在，暂不加入 active 监听集");
                continue;
            }
            match watcher.watch_dir(&dir) {
                Ok(()) => {
                    tracing::info!(dir = %dir.display(), "成功注册目录监听");
                    self.active.insert(dir);
                }
                Err(error) => {
                    tracing::error!(dir = %dir.display(), %error, "注册目录监听失败，保留待重试");
                }
            }
        }

        // 移除监听：active - desired
        let to_remove: Vec<PathBuf> = self.active.difference(desired).cloned().collect();
        for dir in to_remove {
            match watcher.unwatch_dir(&dir) {
                Ok(()) => {
                    tracing::info!(dir = %dir.display(), "成功注销目录监听");
                    self.active.remove(&dir);
                }
                Err(error) => {
                    tracing::warn!(dir = %dir.display(), %error, "注销目录监听失败，保留在 active 待重试");
                }
            }
        }
    }
}

