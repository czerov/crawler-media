//! Cancellation-safe fallback artwork extraction.
use std::path::Path;
use std::process::Command;

use crate::{ProbeTarget, WorkControl};

/// Extract JPEG artwork privately, publishing only while work is still active.
pub fn extract_frame_controlled(
    path: &Path,
    ms: i64,
    out: &Path,
    control: &WorkControl,
) -> Result<(), String> {
    extract_with_program(path, ms, out, control, std::ffi::OsStr::new("ffmpeg"))
}

fn extract_with_program(
    path: &Path,
    ms: i64,
    out: &Path,
    control: &WorkControl,
    program: &std::ffi::OsStr,
) -> Result<(), String> {
    control.check().map_err(|error| error.to_string())?;
    let staging = Staging::new(
        out.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new(".")),
    )?;
    let frame = staging.0.join("frame.jpg");
    let target = ProbeTarget::from_path(path);
    let mut command = Command::new(program);
    command.args(["-y", "-ss", &format!("{:.3}", ms as f64 / 1000.0)]);
    target.apply_ffmpeg_input(&mut command);
    command
        .args(["-frames:v", "1", "-vf", "scale=960:-2", "-q:v", "4"])
        .arg(&frame);
    let output = control.output(&mut command).map_err(|error| {
        tracing::warn!(path = %path.display(), %error, "Controlled frame extraction stopped");
        error.to_string()
    })?;
    if !output.status.success() {
        let detail: String = String::from_utf8_lossy(&output.stderr)
            .chars()
            .take(500)
            .collect();
        tracing::warn!(path = %path.display(), ms, %detail, "ffmpeg frame extraction failed");
        return Err(format!("ffmpeg frame extraction failed: {detail}"));
    }
    control.publish(|| std::fs::rename(&frame, out).map_err(|error| error.to_string()))?;
    tracing::debug!(path = %path.display(), output = %out.display(), "Fallback frame published");
    Ok(())
}

struct Staging(std::path::PathBuf);

impl Staging {
    fn new(parent: &Path) -> Result<Self, String> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        loop {
            let dir = parent.join(format!(
                ".frame-staging-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match std::fs::create_dir(&dir) {
                Ok(()) => return Ok(Self(dir)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.to_string()),
            }
        }
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        let frame = self.0.join("frame.jpg");
        if let Err(error) = std::fs::remove_file(&frame) {
            if error.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(path = %frame.display(), %error, "Frame staging cleanup failed");
            }
        }
        if let Err(error) = std::fs::remove_dir(&self.0) {
            tracing::warn!(path = %self.0.display(), %error, "Frame staging directory cleanup failed");
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, Instant};

    fn executable(dir: &Path, body: &str) -> std::path::PathBuf {
        let path = dir.join("fake-ffmpeg");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn cancellation_reaps_child_without_publishing_frame() {
        let dir = tempfile::tempdir().unwrap();
        let ready = dir.path().join("ready");
        let program = executable(
            dir.path(),
            &format!(
                "for out do :; done\nprintf staged > \"$out\"\nprintf '%s' $$ > '{}'\nexec sleep 30",
                ready.display()
            ),
        );
        let out = dir.path().join("episode-thumb.jpg");
        let control = WorkControl::default();
        let worker_control = control.clone();
        let worker_out = out.clone();
        let worker = std::thread::spawn(move || {
            extract_with_program(
                Path::new("episode.mkv"),
                60_000,
                &worker_out,
                &worker_control,
                program.as_os_str(),
            )
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ready.is_file() {
            assert!(Instant::now() < deadline, "fake ffmpeg did not start");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!out.exists(), "staging must not expose the final sidecar");
        control.cancel();
        assert!(worker.join().unwrap().is_err());
        assert!(
            Instant::now() < deadline,
            "cancellation did not stop the child promptly"
        );
        assert!(!out.exists());
        let pid = std::fs::read_to_string(&ready).unwrap();
        assert!(
            !Command::new("kill")
                .args(["-0", &pid])
                .output()
                .unwrap()
                .status
                .success()
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }

    #[test]
    fn completed_frame_is_published_and_staging_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let program = executable(dir.path(), "for out do :; done\nprintf jpeg > \"$out\"");
        let out = dir.path().join("episode-thumb.jpg");
        extract_with_program(
            Path::new("episode.mkv"),
            60_000,
            &out,
            &WorkControl::default(),
            program.as_os_str(),
        )
        .unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), b"jpeg");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }
}
