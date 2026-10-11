use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::{LibraryError, TransferMode};

pub fn transfer_file(src: &Path, dest: &Path, mode: TransferMode) -> Result<(), LibraryError> {
    transfer_file_controlled(src, dest, mode, &crate::WorkControl::default())
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
    let temp = temporary_path(dest);
    if temp.exists() {
        let _ = fs::remove_file(&temp);
    }

    // 第一阶段：生成暂存文件。Move 模式在暂存阶段先复制，保留原文件 src
    let stage_res = match mode {
        TransferMode::Copy | TransferMode::Move => copy_controlled(src, &temp, control),
        TransferMode::Hardlink => {
            if fs::hard_link(src, &temp).is_err() {
                tracing::warn!(src = %src.display(), dest = %dest.display(), "硬链接失败（可能跨设备），回退为复制");
                copy_controlled(src, &temp, control)
            } else {
                Ok(())
            }
        }
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
    let result = control.publish(|| {
        fs::rename(&temp, dest).map_err(|e| e.to_string())?;
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
        if control.is_cancelled() {
            return Err(LibraryError::Cancelled);
        }
        return Err(LibraryError::Io(std::io::Error::other(error)));
    }
    Ok(())
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
