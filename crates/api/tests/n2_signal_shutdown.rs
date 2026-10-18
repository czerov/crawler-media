//! Review N-2: SIGTERM / SIGINT release the instance lease before exiting, so
//! an immediate restart is writable; `/health` exposes the read-only state.

use std::fs::File;
use std::net::TcpListener;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const TOKEN: &str = "n2-signal-token";

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn start(data_dir: &Path, port: u16) -> Child {
    std::fs::create_dir_all(data_dir).unwrap();
    let out = File::create(data_dir.join("server.log")).unwrap();
    Command::new(env!("CARGO_BIN_EXE_crawler-media"))
        .arg("serve")
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", std::env::var("HOME").unwrap_or_default())
        .env("CRAWLER_MEDIA_DATA", data_dir)
        .env("CRAWLER_MEDIA_TOKEN", TOKEN)
        .env("CRAWLER_MEDIA_ADMIN_PASSWORD", "n2-admin-password")
        .env("CRAWLER_MEDIA_LISTEN", format!("127.0.0.1:{port}"))
        .stdout(Stdio::from(out.try_clone().unwrap()))
        .stderr(Stdio::from(out))
        .spawn()
        .unwrap()
}

fn health(port: u16) -> Option<serde_json::Value> {
    let agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(5)))
        .build()
        .new_agent();
    let mut response = agent
        .get(&format!("http://127.0.0.1:{port}/api/v1/health"))
        .call()
        .ok()?;
    serde_json::from_str(&response.body_mut().read_to_string().ok()?).ok()
}

fn wait_lease_held(child: &mut Child, data_dir: &Path, port: u16) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        assert!(child.try_wait().unwrap().is_none(), "server exited early");
        let held = rusqlite::Connection::open(data_dir.join("app.db"))
            .ok()
            .and_then(|c| {
                c.query_row("SELECT COUNT(*) FROM instance_lock", [], |r| {
                    r.get::<_, i64>(0)
                })
                .ok()
            })
            == Some(1);
        if held && let Some(body) = health(port) {
            assert_eq!(body["data"]["read_only"], false, "{body}");
            return;
        }
        assert!(Instant::now() < deadline, "lease not taken in time");
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn signal_and_check_release(signal: &str) {
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
    let port = free_port();
    let mut child = start(&data_dir, port);
    wait_lease_held(&mut child, &data_dir, port);
    let status = Command::new("kill")
        .args([&format!("-{signal}"), &child.id().to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    let deadline = Instant::now() + Duration::from_secs(15);
    let exit = loop {
        if let Some(exit) = child.try_wait().unwrap() {
            break exit;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            panic!("{signal} did not stop the server");
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(exit.success(), "{signal}: clean exit, got {exit}");
    let rows: i64 = rusqlite::Connection::open(data_dir.join("app.db"))
        .unwrap()
        .query_row("SELECT COUNT(*) FROM instance_lock", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 0, "{signal}: lease released on shutdown");
}

#[test]
fn sigterm_releases_the_instance_lease() {
    signal_and_check_release("TERM");
}

#[test]
fn sigint_releases_the_instance_lease() {
    signal_and_check_release("INT");
}
