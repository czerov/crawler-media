//! Controlled 字幕提取回归：唯一 staging、取消语义与发布互不污染。
//!
//! 测试不启动任何真实进程：`FakeRunner` 读取 `Command::get_args()` 的最后一个
//! 参数（约定为输出 staging path）并直接写 fixture bytes。

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Output};
use std::sync::{Arc, Condvar, Mutex};

use library::{
    ControlledSubtitleRunner, DeliveryError, LibraryError, SubtitleRunner, SubtitleTrack, Tracks,
    WorkControl, deliver_subtitle_controlled,
};

const STREAM_INDEX: u32 = 2;

fn embedded_tracks() -> Tracks {
    Tracks {
        subtitles: vec![SubtitleTrack {
            stream_index: Some(STREAM_INDEX),
            codec: Some("subrip".into()),
            is_external: false,
            path: None,
            ..Default::default()
        }],
        ..Default::default()
    }
}

fn exit_status(code: i32) -> ExitStatus {
    use std::os::unix::process::ExitStatusExt;
    ExitStatus::from_raw(code << 8)
}

fn success_output() -> Output {
    Output {
        status: exit_status(0),
        stdout: Vec::new(),
        stderr: Vec::new(),
    }
}

fn failure_output(stderr: &[u8]) -> Output {
    Output {
        status: exit_status(1),
        stdout: Vec::new(),
        stderr: stderr.to_vec(),
    }
}

/// 让提取停在闭包内，便于在“提取中”取消。
struct Gate {
    entered: Mutex<bool>,
    entered_cv: Condvar,
    open: Mutex<bool>,
    open_cv: Condvar,
}

impl Gate {
    fn new() -> Self {
        Self {
            entered: Mutex::new(false),
            entered_cv: Condvar::new(),
            open: Mutex::new(false),
            open_cv: Condvar::new(),
        }
    }

    fn enter_and_wait(&self) {
        *self.entered.lock().unwrap() = true;
        self.entered_cv.notify_all();
        let mut open = self.open.lock().unwrap();
        while !*open {
            open = self.open_cv.wait(open).unwrap();
        }
    }

    fn wait_entered(&self) {
        let mut entered = self.entered.lock().unwrap();
        while !*entered {
            entered = self.entered_cv.wait(entered).unwrap();
        }
    }

    fn release(&self) {
        *self.open.lock().unwrap() = true;
        self.open_cv.notify_all();
    }
}

#[derive(Default)]
struct FakeRunner {
    gate: Option<Arc<Gate>>,
    body: Vec<u8>,
    /// 返回非零退出状态，且不写 staging 文件。
    fail: bool,
    /// 写完成 staging 后取消 control，模拟“提取完成后才发现取消”。
    cancel_after_output: bool,
    temps: Mutex<Vec<PathBuf>>,
}

impl FakeRunner {
    fn writing(body: &[u8]) -> Self {
        Self {
            body: body.to_vec(),
            ..Default::default()
        }
    }

    fn gated(gate: Arc<Gate>, body: &[u8]) -> Self {
        Self {
            gate: Some(gate),
            body: body.to_vec(),
            ..Default::default()
        }
    }

    fn failing() -> Self {
        Self {
            fail: true,
            ..Default::default()
        }
    }

    fn cancelling_after_output(body: &[u8]) -> Self {
        Self {
            cancel_after_output: true,
            body: body.to_vec(),
            ..Default::default()
        }
    }

    fn temps(&self) -> Vec<PathBuf> {
        self.temps.lock().unwrap().clone()
    }
}

impl SubtitleRunner for FakeRunner {
    fn output(&self, command: &mut Command, control: &WorkControl) -> Result<Output, LibraryError> {
        let staged = PathBuf::from(
            command
                .get_args()
                .last()
                .expect("command must end with the staging output path"),
        );
        self.temps.lock().unwrap().push(staged.clone());

        if let Some(gate) = &self.gate {
            gate.enter_and_wait();
        }
        if control.is_cancelled() {
            return Err(LibraryError::Cancelled);
        }
        if self.fail {
            return Ok(failure_output(b"fixture ffmpeg failure"));
        }
        std::fs::write(&staged, &self.body).unwrap();
        if self.cancel_after_output {
            control.cancel();
        }
        Ok(success_output())
    }
}

fn fixture(root: &Path) -> (PathBuf, PathBuf) {
    let source = root.join("movie.mkv");
    std::fs::write(&source, b"fake video bytes").unwrap();
    let cache = root.join("cache");
    (source, cache)
}

#[test]
fn successful_extraction_publishes_and_removes_staging() {
    let tmp = tempfile::tempdir().unwrap();
    let (source, cache) = fixture(tmp.path());
    let runner = FakeRunner::writing(b"EXTRACTED-VTT");

    let payload = deliver_subtitle_controlled(
        &embedded_tracks(),
        STREAM_INDEX,
        true,
        Some(&source),
        Some(&cache),
        &WorkControl::default(),
        &runner,
    )
    .unwrap();

    assert_eq!(payload.content_type, "text/vtt; charset=utf-8");
    assert_eq!(payload.bytes, b"EXTRACTED-VTT");
    assert!(cache.join("sub_2.vtt").is_file());
    let temps = runner.temps();
    assert_eq!(temps.len(), 1);
    assert!(!temps[0].exists(), "published staging must be renamed away");
    assert!(
        temps[0]
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("sub_2_"),
        "staging must be uniquely named: {:?}",
        temps[0]
    );
}

#[test]
fn cancelled_extraction_leaves_no_final_cache() {
    let tmp = tempfile::tempdir().unwrap();
    let (source, cache) = fixture(tmp.path());
    let gate = Arc::new(Gate::new());
    let runner = FakeRunner::gated(gate.clone(), b"SHOULD-NOT-PUBLISH");
    let control = WorkControl::default();

    let result = std::thread::scope(|scope| {
        let handle = scope.spawn(|| {
            deliver_subtitle_controlled(
                &embedded_tracks(),
                STREAM_INDEX,
                true,
                Some(&source),
                Some(&cache),
                &control,
                &runner,
            )
        });
        gate.wait_entered();
        control.cancel();
        gate.release();
        handle.join().unwrap()
    });

    assert!(result.is_err());
    assert!(!cache.join("sub_2.vtt").exists());
    for staged in runner.temps() {
        assert!(!staged.exists(), "cancelled staging must be removed");
    }
}

#[test]
fn cancel_after_extraction_does_not_publish() {
    let tmp = tempfile::tempdir().unwrap();
    let (source, cache) = fixture(tmp.path());
    let runner = FakeRunner::cancelling_after_output(b"LATE-BODY");

    let result = deliver_subtitle_controlled(
        &embedded_tracks(),
        STREAM_INDEX,
        true,
        Some(&source),
        Some(&cache),
        &WorkControl::default(),
        &runner,
    );

    assert!(result.is_err(), "publish-time cancellation must fail");
    assert!(!cache.join("sub_2.vtt").exists());
    for staged in runner.temps() {
        assert!(!staged.exists(), "cancelled staging must be removed");
    }
}

#[test]
fn cancelled_request_does_not_serve_warm_cache() {
    let tmp = tempfile::tempdir().unwrap();
    let (source, cache) = fixture(tmp.path());
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::write(cache.join("sub_2.vtt"), b"WARM-CACHE").unwrap();
    let runner = FakeRunner::writing(b"NEVER-RUN");
    let control = WorkControl::default();
    control.cancel();

    let result = deliver_subtitle_controlled(
        &embedded_tracks(),
        STREAM_INDEX,
        true,
        Some(&source),
        Some(&cache),
        &control,
        &runner,
    );

    assert!(result.is_err());
    assert!(
        runner.temps().is_empty(),
        "cancelled request must not run ffmpeg"
    );
}

#[test]
fn same_index_different_formats_use_distinct_staging() {
    let tmp = tempfile::tempdir().unwrap();
    let (source, cache) = fixture(tmp.path());
    let tracks = embedded_tracks();
    let srt_runner = FakeRunner::writing(b"SRT-BODY");
    let vtt_runner = FakeRunner::writing(b"VTT-BODY");

    let (srt, vtt) = std::thread::scope(|scope| {
        let srt_handle = scope.spawn(|| {
            deliver_subtitle_controlled(
                &tracks,
                STREAM_INDEX,
                false,
                Some(&source),
                Some(&cache),
                &WorkControl::default(),
                &srt_runner,
            )
            .unwrap()
        });
        let vtt_handle = scope.spawn(|| {
            deliver_subtitle_controlled(
                &tracks,
                STREAM_INDEX,
                true,
                Some(&source),
                Some(&cache),
                &WorkControl::default(),
                &vtt_runner,
            )
            .unwrap()
        });
        (srt_handle.join().unwrap(), vtt_handle.join().unwrap())
    });

    assert_eq!(srt.content_type, "text/plain; charset=utf-8");
    assert_eq!(srt.bytes, b"SRT-BODY");
    assert_eq!(vtt.content_type, "text/vtt; charset=utf-8");
    assert_eq!(vtt.bytes, b"VTT-BODY");
    assert_eq!(std::fs::read(cache.join("sub_2.srt")).unwrap(), b"SRT-BODY");
    assert_eq!(std::fs::read(cache.join("sub_2.vtt")).unwrap(), b"VTT-BODY");

    let srt_temps = srt_runner.temps();
    let vtt_temps = vtt_runner.temps();
    assert_eq!(srt_temps.len(), 1);
    assert_eq!(vtt_temps.len(), 1);
    assert_ne!(
        srt_temps[0], vtt_temps[0],
        "concurrent formats must not share a staging path"
    );
}

#[test]
fn failed_extraction_leaves_no_late_cache() {
    let tmp = tempfile::tempdir().unwrap();
    let (source, cache) = fixture(tmp.path());
    let runner = FakeRunner::failing();

    let result = deliver_subtitle_controlled(
        &embedded_tracks(),
        STREAM_INDEX,
        false,
        Some(&source),
        Some(&cache),
        &WorkControl::default(),
        &runner,
    );

    assert!(matches!(result, Err(DeliveryError::Extraction(_))));
    assert!(!cache.join("sub_2.srt").exists());
    for staged in runner.temps() {
        assert!(!staged.exists(), "failed staging must be removed");
    }
}

#[test]
fn embedded_track_requires_stream_index_not_ordinal() {
    let tmp = tempfile::tempdir().unwrap();
    let (source, cache) = fixture(tmp.path());
    let tracks = embedded_tracks();
    let runner = FakeRunner::writing(b"BY-STREAM-INDEX");

    // 内封轨的请求 index 是 stream_index(2)，ordinal 0 不是合法请求。
    let ordinal = deliver_subtitle_controlled(
        &tracks,
        0,
        false,
        Some(&source),
        Some(&cache),
        &WorkControl::default(),
        &runner,
    );
    assert!(matches!(ordinal, Err(DeliveryError::TrackNotFound)));

    let payload = deliver_subtitle_controlled(
        &tracks,
        STREAM_INDEX,
        false,
        Some(&source),
        Some(&cache),
        &WorkControl::default(),
        &runner,
    )
    .unwrap();
    assert_eq!(payload.bytes, b"BY-STREAM-INDEX");
    assert!(cache.join("sub_2.srt").is_file());
}

#[test]
fn production_runner_rejects_cancelled_work_without_spawning() {
    let runner = ControlledSubtitleRunner;
    let mut command = Command::new("ffmpeg-not-installed-in-tests");
    let control = WorkControl::default();
    control.cancel();
    // check() 先于 spawn：取消时不会启动任何进程。
    assert!(matches!(
        runner.output(&mut command, &control),
        Err(LibraryError::Cancelled)
    ));
}
