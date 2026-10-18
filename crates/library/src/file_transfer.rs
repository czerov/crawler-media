use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::{LibraryError, TransferMode};

pub fn transfer_file(src: &Path, dest: &Path, mode: TransferMode) -> Result<(), LibraryError> {
    transfer_file_controlled(src, dest, mode, &crate::WorkControl::default())
}

static HARDLINK_FALLBACK_COPY: AtomicBool = AtomicBool::new(true);
static HARDLINK_FALLBACKS: AtomicU64 = AtomicU64::new(0);

/// D-M2: whether a failed hardlink falls back to copying (default true).
pub fn set_hardlink_fallback_copy(allow: bool) {
    HARDLINK_FALLBACK_COPY.store(allow, Ordering::Relaxed);
}

/// D-M2: process-wide count of hardlink → copy fallbacks.
pub fn hardlink_fallback_count() -> u64 {
    HARDLINK_FALLBACKS.load(Ordering::Relaxed)
}

pub fn transfer_file_controlled(
    src: &Path,
    dest: &Path,
    mode: TransferMode,
    control: &crate::WorkControl,
) -> Result<(), LibraryError> {
    control.check()?;
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    if dest.is_dir() {
        return Err(LibraryError::Io(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!(
                "destination path is an existing directory: {}",
                dest.display()
            ),
        )));
    }
    // 源文件与目标文件若是同一个规范化路径（或指向同一硬链接/符号链接实体），直接作为 no-op 成功返回
    if src == dest {
        return Ok(());
    }
    if let (Ok(src_canon), Ok(dest_canon)) = (src.canonicalize(), dest.canonicalize()) {
        if src_canon == dest_canon {
            return Ok(());
        }
    }
    if same_inode(src, dest) {
        // `dest` already is this very file (e.g. a hardlink from a previous run).
        return Ok(());
    }
    let temp = temporary_path(dest);
    if temp.exists() {
        let _ = fs::remove_file(&temp);
    }

    // 第一阶段：生成暂存文件。Move 模式在暂存阶段先复制，保留原文件 src
    let stage_res = match mode {
        TransferMode::Copy => copy_controlled(src, &temp, control),
        // D-M1: same device → link the very inode (no data copy); the source
        // name is removed after publication, so Move is effectively a rename.
        TransferMode::Move => {
            if fs::hard_link(src, &temp).is_ok() {
                Ok(())
            } else {
                copy_controlled(src, &temp, control)
            }
        }
        TransferMode::Hardlink => match fs::hard_link(src, &temp) {
            Ok(()) => Ok(()),
            Err(error) if !HARDLINK_FALLBACK_COPY.load(Ordering::Relaxed) => {
                Err(LibraryError::Io(std::io::Error::new(
                    error.kind(),
                    format!(
                        "硬链接失败且已禁用复制回退（下载目录与媒体库可能不在同一设备）: {} -> {}: {error}",
                        src.display(),
                        dest.display()
                    ),
                )))
            }
            Err(_) => {
                HARDLINK_FALLBACKS.fetch_add(1, Ordering::Relaxed);
                tracing::warn!(src = %src.display(), dest = %dest.display(), "硬链接失败（可能跨设备），回退为复制");
                copy_controlled(src, &temp, control)
            }
        },
    };
    if let Err(e) = stage_res {
        if temp.exists() {
            let _ = fs::remove_file(&temp);
        }
        return Err(e);
    }

    if let Err(error) = control.check() {
        let _ = fs::remove_file(&temp);
        return Err(error);
    }
    // The shared publication gate linearizes final rename/source removal with cancellation.
    // Publication never overwrites an existing destination (D-H3): a file that
    // appeared at `dest` after the caller's existence check is left untouched
    // and reported as `AlreadyExists` so the caller can pick another name.
    let mut clobber: Option<std::io::Error> = None;
    let result = control.publish(|| {
        if let Err(error) = publish_no_clobber(&temp, dest) {
            let message = error.to_string();
            clobber = Some(error);
            return Err(message);
        }
        if mode == TransferMode::Move {
            if let Err(error) = fs::remove_file(src) {
                tracing::warn!(src = %src.display(), dest = %dest.display(), %error,
                    "文件已发布，但清理源文件失败");
            }
        }
        Ok(())
    });
    if let Err(error) = result {
        let _ = fs::remove_file(&temp);
        if let Some(io) = clobber {
            return Err(LibraryError::Io(io));
        }
        if control.is_cancelled() {
            return Err(LibraryError::Cancelled);
        }
        return Err(LibraryError::Io(std::io::Error::other(error)));
    }
    Ok(())
}

/// Move the fully staged `temp` to `dest` without ever replacing `dest`.
/// `hard_link` is an atomic "create if absent" on POSIX filesystems; where
/// links are unsupported, fall back to an existence check plus rename.
fn publish_no_clobber(temp: &Path, dest: &Path) -> std::io::Result<()> {
    match fs::hard_link(temp, dest) {
        Ok(()) => {
            let _ = fs::remove_file(temp);
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            Err(already_exists(dest))
        }
        Err(_) => {
            if fs::symlink_metadata(dest).is_ok() {
                return Err(already_exists(dest));
            }
            fs::rename(temp, dest)
        }
    }
}

fn already_exists(dest: &Path) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        format!(
            "destination already exists, refusing to overwrite: {}",
            dest.display()
        ),
    )
}

fn temporary_path(dest: &Path) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nonce = NEXT.fetch_add(1, Ordering::Relaxed);
    let mut name = dest
        .file_name()
        .unwrap_or_else(|| "media".as_ref())
        .to_os_string();
    name.push(format!(".transfer-{}-{nonce}", std::process::id()));
    dest.with_file_name(name)
}

fn copy_controlled(
    src: &Path,
    dest: &Path,
    control: &crate::WorkControl,
) -> Result<(), LibraryError> {
    let mut source = fs::File::open(src)?;
    let mut target = fs::File::create(dest)?;
    let mut buffer = vec![0; 1024 * 1024];
    loop {
        control.check()?;
        let count = source.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        control.check()?;
        target.write_all(&buffer[..count])?;
    }
    target.set_permissions(source.metadata()?.permissions())?;
    control.check()?;
    Ok(())
}

#[cfg(unix)]
fn same_inode(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (fs::metadata(a), fs::metadata(b)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

#[cfg(not(unix))]
fn same_inode(_: &Path, _: &Path) -> bool {
    false
}
