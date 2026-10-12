//! Library 封面渲染与版本化发布（C2b）。
//!
//! 渲染结果先写成新的不可变版本文件 `library-{id}-{uuid}.jpg`，**不覆盖**当前
//! 文件；DB 指针通过 `Store::compare_set_library_cover` 的 revision CAS 最后发布。
//! CAS 未命中或 DB 写失败时删除这个尚未被引用的新版本并返回错误。
//! `preview_only` 只返回字节，不写文件、不写 DB、不动 revision。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use cover_generator::CoverOptions;

use crate::ApiState;

pub const MARKER_CHECKED: &str = "checked-v2";
pub const MARKER_MANUAL: &str = "manual-v2";
pub const MARKER_RETRY: &str = "auto-v2";

/// 锁内取到的 owned 事实快照；渲染/写盘全部在锁外进行。
pub struct CoverSnapshot {
    pub library: crate::store::Library,
    pub rows: Vec<domain::LedgerRow>,
    pub revision: i64,
    pub marker: Option<String>,
    pub target_dir: PathBuf,
}

pub trait CoverRenderer: Send + Sync {
    fn render(
        &self,
        library: &crate::store::Library,
        rows: &[domain::LedgerRow],
        options: &CoverOptions,
    ) -> Result<Option<Vec<u8>>, String>;
}

/// 生产渲染器：读取本地 poster.jpg 并用 cover-generator 合成。
pub struct LocalCoverRenderer;

impl CoverRenderer for LocalCoverRenderer {
    fn render(
        &self,
        library: &crate::store::Library,
        rows: &[domain::LedgerRow],
        options: &CoverOptions,
    ) -> Result<Option<Vec<u8>>, String> {
        crate::http::library::render_cover_jpeg(library, rows, options)
    }
}

pub struct GeneratedCover {
    pub bytes: Vec<u8>,
    pub published_path: Option<PathBuf>,
}

pub struct LibraryCoverService {
    renderer: Arc<dyn CoverRenderer>,
}

impl LibraryCoverService {
    pub fn new(renderer: Arc<dyn CoverRenderer>) -> Self {
        Self { renderer }
    }

    /// 生产默认服务（真实渲染器）。
    pub fn local() -> Self {
        Self::new(Arc::new(LocalCoverRenderer))
    }

    /// 自动封面：先取自身已存在的版本；没有才渲染并 CAS 发布。
    pub async fn ensure_cover(
        &self,
        state: &ApiState,
        id: &str,
    ) -> Result<Option<PathBuf>, String> {
        // Registry → Store 锁序由 acquire_library_work 保证；lease 活到真实工作结束。
        let lease = state.acquire_library_work(id)?;
        let snapshot = snapshot(state, id)?;
        // 显式删除（none/空串）后绝不自动回填。
        if matches!(
            snapshot.library.cover_path.as_deref(),
            Some("none") | Some("")
        ) {
            return Ok(None);
        }
        if let Some(existing) = existing_cover(&snapshot) {
            if !needs_legacy_upgrade(&snapshot, &existing) {
                return Ok(Some(existing));
            }
            // 兼容路径：legacy 固定路径里的 fanart 副本就地升级（不 unlink、不新建
            // 版本文件），指针仍走 revision CAS。
            let options = crate::http::library::default_cover_options(&snapshot.library);
            let rendered = render_off_lock(self.renderer.clone(), &snapshot, options).await?;
            let Some(bytes) = rendered else {
                mark(state, &snapshot.library.id, MARKER_RETRY);
                return Ok(Some(existing));
            };
            let target = existing.clone();
            write_bytes(&target, bytes).await?;
            let published = cas_publish(state, &snapshot, &target, MARKER_CHECKED, &lease)?;
            if !published {
                return Err("cover.stale: 封面已被更新的修改覆盖".into());
            }
            mark(state, &snapshot.library.id, MARKER_CHECKED);
            return Ok(Some(target));
        }
        if snapshot.marker.as_deref() == Some(MARKER_RETRY) {
            return Ok(None);
        }
        let options = crate::http::library::default_cover_options(&snapshot.library);
        let rendered = render_off_lock(self.renderer.clone(), &snapshot, options).await?;
        let Some(bytes) = rendered else {
            mark(state, &snapshot.library.id, MARKER_RETRY);
            return Ok(None);
        };
        let target = publish(state, &snapshot, &bytes, MARKER_CHECKED, &lease).await?;
        mark(state, &snapshot.library.id, MARKER_CHECKED);
        Ok(Some(target))
    }

    /// 手动上传：新版本文件 + CAS 发布，标记 manual。
    pub async fn replace_cover(
        &self,
        state: &ApiState,
        id: &str,
        bytes: Vec<u8>,
    ) -> Result<PathBuf, String> {
        let lease = state.acquire_library_work(id)?;
        let snapshot = snapshot(state, id)?;
        publish(state, &snapshot, &bytes, MARKER_MANUAL, &lease).await
    }

    /// 手动/预览生成：`preview_only` 不落盘、不动 DB。
    pub async fn generate(
        &self,
        state: &ApiState,
        id: &str,
        options: CoverOptions,
        preview_only: bool,
    ) -> Result<GeneratedCover, String> {
        let lease = state.acquire_library_work(id)?;
        let snapshot = snapshot(state, id)?;
        let rendered = render_off_lock(self.renderer.clone(), &snapshot, options).await?;
        let Some(bytes) = rendered else {
            return Err("cover.empty: 媒体库内无可用海报图片".into());
        };
        if preview_only {
            return Ok(GeneratedCover {
                bytes,
                published_path: None,
            });
        }
        let target = publish(state, &snapshot, &bytes, MARKER_MANUAL, &lease).await?;
        Ok(GeneratedCover {
            bytes,
            published_path: Some(target),
        })
    }

    /// 清空封面：CAS 到 `none` 并推进 revision，晚到的自动渲染因此 CAS 失败。
    pub async fn remove_cover(&self, state: &ApiState, id: &str) -> Result<(), String> {
        let lease = state.acquire_library_work(id)?;
        let snapshot = snapshot(state, id)?;
        let marker = snapshot
            .marker
            .clone()
            .unwrap_or_else(|| MARKER_CHECKED.into());
        let previous = existing_cover(&snapshot);
        let published = lease.publish(|| {
            let store = state.store.lock();
            store
                .compare_set_library_cover(
                    &snapshot.library.id,
                    snapshot.revision,
                    Some("none"),
                    &marker,
                )
                .map_err(|error| error.to_string())
        })?;
        if !published {
            return Err("cover.stale: 封面已被更新，请重试".into());
        }
        // 只清理我们发布的版本文件；legacy 固定路径保持只读、不 unlink。
        if let Some(path) = previous {
            if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(&format!("library-{}-", snapshot.library.id)))
            {
                let _ = std::fs::remove_file(path);
            }
        }
        Ok(())
    }
}

/// 锁内只取 owned DB 事实。
pub fn snapshot(state: &ApiState, id: &str) -> Result<CoverSnapshot, String> {
    let store = state.store.lock();
    let library = store
        .get_library(id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "library.missing: 媒体库不存在".to_string())?;
    let rows = crate::http::library::rows_in_library(&store, &library);
    let marker = store
        .get_setting(&format!("library_cover_checked.{}", library.id))
        .ok()
        .flatten();
    Ok(CoverSnapshot {
        revision: library.cover_revision,
        marker,
        target_dir: store.library_covers_dir(),
        library,
        rows,
    })
}

/// 当前已存在的封面（只读）：`none`/空串表示显式删除。
pub fn existing_cover(snapshot: &CoverSnapshot) -> Option<PathBuf> {
    match snapshot.library.cover_path.as_deref() {
        Some("none") | Some("") => return None,
        Some(path) => {
            let path = PathBuf::from(path);
            if path.is_file() {
                return Some(path);
            }
        }
        None => {}
    }
    let legacy = snapshot
        .target_dir
        .join(format!("library-{}.jpg", snapshot.library.id));
    legacy.is_file().then_some(legacy)
}

/// 旧 release 把单张 fanart 复制成固定路径封面：只有这种形态才重渲染升级。
fn needs_legacy_upgrade(snapshot: &CoverSnapshot, existing: &Path) -> bool {
    let legacy = snapshot
        .target_dir
        .join(format!("library-{}.jpg", snapshot.library.id));
    existing == legacy
        && snapshot.marker.as_deref() != Some(MARKER_CHECKED)
        && !snapshot.rows.is_empty()
        && crate::http::library::is_legacy_fanart_copy(&snapshot.rows, existing)
}

/// 写入一个已存在路径（legacy 就地升级专用）。
async fn write_bytes(target: &Path, bytes: Vec<u8>) -> Result<(), String> {
    let target = target.to_path_buf();
    tokio::task::spawn_blocking(move || {
        std::fs::write(&target, bytes).map_err(|error| format!("cover.write: {error}"))
    })
    .await
    .map_err(|error| format!("cover.write: {error}"))?
}

/// `lease.publish` 内只做 CAS；命中返回 true。
fn cas_publish(
    state: &ApiState,
    snapshot: &CoverSnapshot,
    target: &Path,
    marker: &str,
    lease: &crate::library_lifecycle::LibraryWorkLease,
) -> Result<bool, String> {
    lease.publish(|| {
        let store = state.store.lock();
        store
            .compare_set_library_cover(
                &snapshot.library.id,
                snapshot.revision,
                Some(&target.display().to_string()),
                marker,
            )
            .map_err(|error| error.to_string())
    })
}

async fn render_off_lock(
    renderer: Arc<dyn CoverRenderer>,
    snapshot: &CoverSnapshot,
    options: CoverOptions,
) -> Result<Option<Vec<u8>>, String> {
    let library = snapshot.library.clone();
    let rows = snapshot.rows.clone();
    tokio::task::spawn_blocking(move || renderer.render(&library, &rows, &options))
        .await
        .map_err(|error| format!("cover.render: {error}"))?
}

/// 写新的不可变版本文件，然后在 `lease.publish` 内做 CAS；未命中/失败即删除新版本。
async fn publish(
    state: &ApiState,
    snapshot: &CoverSnapshot,
    bytes: &[u8],
    marker: &str,
    lease: &crate::library_lifecycle::LibraryWorkLease,
) -> Result<PathBuf, String> {
    let target = snapshot.target_dir.join(format!(
        "library-{}-{}.jpg",
        snapshot.library.id,
        uuid::Uuid::new_v4()
    ));
    let write_target = target.clone();
    let write_bytes = bytes.to_vec();
    let dir = snapshot.target_dir.clone();
    tokio::task::spawn_blocking(move || -> Result<(), String> {
        std::fs::create_dir_all(&dir).map_err(|error| format!("cover.write: {error}"))?;
        std::fs::write(&write_target, write_bytes).map_err(|error| format!("cover.write: {error}"))
    })
    .await
    .map_err(|error| format!("cover.write: {error}"))??;

    let published = cas_publish(state, snapshot, &target, marker, lease);
    match published {
        Ok(true) => {
            tracing::info!(
                library_id = %snapshot.library.id,
                cover_path = %target.display(),
                revision = snapshot.revision + 1,
                "published library cover revision"
            );
            Ok(target)
        }
        Ok(false) => {
            discard_unreferenced(&target);
            Err("cover.stale: 封面已被更新的修改覆盖，已丢弃本次结果".into())
        }
        Err(error) => {
            discard_unreferenced(&target);
            Err(error)
        }
    }
}

fn discard_unreferenced(target: &Path) {
    if let Err(error) = std::fs::remove_file(target) {
        tracing::warn!(%error, path = %target.display(), "failed to discard unreferenced cover version");
    }
}

fn mark(state: &ApiState, library_id: &str, marker: &str) {
    let store = state.store.lock();
    if let Err(error) = store.put_setting(&format!("library_cover_checked.{library_id}"), marker) {
        tracing::warn!(%error, library_id, "failed to mark library cover state");
    }
}
