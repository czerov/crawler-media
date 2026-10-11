use std::process::Command;
use library::WorkControl;

#[test]
fn cancelled_control_never_launches_a_process() {
    let control = WorkControl::default();
    control.cancel();
    assert!(control.check().is_err());
    assert!(control.output(&mut Command::new("nonexistent-cancelled-command")).is_err());
}

#[test]
fn child_helper() {
    if std::env::var_os("LIBRARY_WORK_CONTROL_CHILD").is_some() {
        std::fs::write(std::env::var_os("LIBRARY_WORK_CONTROL_READY").unwrap(), b"ready").unwrap();
        loop { std::thread::park(); }
    }
}

#[test]
fn cancellation_reaps_a_running_child() {
    let control = WorkControl::default();
    let cancel = control.clone();
    let temp = tempfile::tempdir().unwrap();
    let ready = temp.path().join("ready");
    let child_ready = ready.clone();
    let worker = std::thread::spawn(move || {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command.args(["--exact", "child_helper", "--nocapture"])
            .env("LIBRARY_WORK_CONTROL_CHILD", "1")
            .env("LIBRARY_WORK_CONTROL_READY", child_ready);
        control.output(&mut command)
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !ready.is_file() {
        if std::time::Instant::now() >= deadline {
            cancel.cancel();
            let _ = worker.join();
            panic!("child did not signal readiness");
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    cancel.cancel();
    assert!(matches!(worker.join().unwrap(), Err(library::LibraryError::Cancelled)));
}

#[test]
fn output_captures_both_pipes() {
    let result = WorkControl::default().output(
        Command::new(std::env::current_exe().unwrap()).args(["--exact", "child_helper"])
    ).unwrap();
    assert!(result.status.success());
    assert!(String::from_utf8_lossy(&result.stdout).contains("test result: ok"));
}
