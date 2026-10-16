//! D-H1: QbitDownloader renews an expired session once on 403.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use downloader::{Downloader, QbitConfig, QbitDownloader};

struct Fake {
    logins: AtomicUsize,
    forbid: AtomicUsize,
}

fn serve(fake: Arc<Fake>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut buf = [0u8; 16384];
            let n = stream.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let (status, body, extra) = if req.starts_with("POST /api/v2/auth/login") {
                let n = fake.logins.fetch_add(1, Ordering::SeqCst);
                ("200 OK", "Ok.".to_string(), format!("Set-Cookie: SID=sid-{n}; HttpOnly\r\n"))
            } else if fake.forbid.try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1)).is_ok() {
                ("403 Forbidden", "Forbidden".into(), String::new())
            } else {
                ("200 OK", "[]".into(), String::new())
            };
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });
    url
}

fn connect(url: String) -> QbitDownloader {
    QbitDownloader::connect(QbitConfig {
        url,
        username: "admin".into(),
        password: "pass".into(),
        category: None,
        path_maps: vec![],
    })
    .unwrap()
}

#[test]
fn single_403_triggers_exactly_one_relogin_and_retry() {
    let fake = Arc::new(Fake { logins: AtomicUsize::new(0), forbid: AtomicUsize::new(0) });
    let dl = connect(serve(fake.clone()));
    fake.forbid.store(1, Ordering::SeqCst);
    assert!(dl.task_snapshots().unwrap().is_empty());
    assert_eq!(fake.logins.load(Ordering::SeqCst), 2, "connect + one re-login");
    assert!(dl.task_snapshots().unwrap().is_empty());
    assert_eq!(fake.logins.load(Ordering::SeqCst), 2, "renewed cookie is reused");
}

#[test]
fn persistent_403_is_an_authentication_error_without_loops() {
    let fake = Arc::new(Fake { logins: AtomicUsize::new(0), forbid: AtomicUsize::new(0) });
    let dl = connect(serve(fake.clone()));
    fake.forbid.store(10, Ordering::SeqCst);
    let error = dl.task_snapshots().unwrap_err().to_string();
    assert!(error.contains("authentication failed"), "{error}");
    assert_eq!(fake.logins.load(Ordering::SeqCst), 2);
    assert_eq!(fake.forbid.load(Ordering::SeqCst), 8, "exactly two attempts");
}
