use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use downloader::{QbitConfig, QbitDownloader, TransmissionConfig, TransmissionDownloader};

const CHILD_ENV: &str = "CRAWLER_MEDIA_LOCAL_PROXY_TEST_CHILD";
const TEST_NAME: &str = "loopback_downloader_connections_bypass_system_proxy";
/// 读取请求的最长等待。loopback 请求实际只需微秒级，这里只是为了不让 worker
/// 永久阻塞在半个请求上；客户端自身的全局超时也是 5s。
const REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(5);

/// `accept()` 返回的 stream 会**继承 listener 的非阻塞标志**（macOS/BSD 行为）。
///
/// 不切回阻塞时，客户端请求还没到达的那一刻 `read` 会立刻返回 `WouldBlock`，
/// 脚手架会把它误当成 EOF：请求被读成 0 字节、服务端提前回包并关闭连接，
/// 客户端偶发 `io: Invalid argument (os error 22)`（DB15 flaky）。
/// 切回阻塞 + 读超时后，服务端会读到**完整请求**或超时为止。
fn prepare_stream(stream: &mut TcpStream) {
    stream.set_nonblocking(false).unwrap();
    let _ = stream.set_read_timeout(Some(REQUEST_READ_TIMEOUT));
}

struct Server {
    url: String,
    stop: Arc<AtomicBool>,
    served: Arc<AtomicUsize>,
    worker: JoinHandle<()>,
}

impl Server {
    /// loopback HTTP 服务端：**服务到 [`Server::close`] 为止**（不是只接一次连接）。
    fn new(status: &'static str, headers: &'static str, body: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let served = Arc::new(AtomicUsize::new(0));
        let worker_served = served.clone();
        let worker = thread::spawn(move || {
            while !worker_stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        prepare_stream(&mut stream);
                        // 只有完整读到请求才算服务成功；半截请求（客户端还没写完）
                        // 不允许被当成"服务过了"。
                        if read_request(&mut stream).is_some() {
                            worker_served.fetch_add(1, Ordering::SeqCst);
                        }
                        let response = format!(
                            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n{body}",
                            body.len()
                        );
                        let _ = stream.write_all(response.as_bytes());
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            url: format!("http://{addr}"),
            stop,
            served,
            worker,
        }
    }

    /// 关闭服务端，返回它**完整读到**的请求数。
    fn close(self) -> usize {
        self.stop.store(true, Ordering::Relaxed);
        self.worker.join().unwrap();
        self.served.load(Ordering::SeqCst)
    }
}

struct ProxyServer {
    url: String,
    hits: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    worker: JoinHandle<()>,
}

impl ProxyServer {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let worker_hits = hits.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let worker = thread::spawn(move || {
            while !worker_stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        prepare_stream(&mut stream);
                        worker_hits.fetch_add(1, Ordering::Relaxed);
                        let _ = read_request(&mut stream);
                        let _ = stream.write_all(
                            b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        );
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            url: format!("http://{addr}"),
            hits,
            stop,
            worker,
        }
    }

    fn close(self) -> usize {
        self.stop.store(true, Ordering::Relaxed);
        self.worker.join().unwrap();
        self.hits.load(Ordering::Relaxed)
    }
}

/// 读一个**完整**的 HTTP 请求（请求头 + Content-Length 指定的 body）。
///
/// 返回 `None` 表示没读到完整请求（对端关闭 / 读超时 / 连接重置），
/// 调用方不应把它算作一次成功的服务。
fn read_request(stream: &mut TcpStream) -> Option<Vec<u8>> {
    let mut request = Vec::new();
    let mut expected = None;
    loop {
        let mut buffer = [0; 1024];
        let read = match stream.read(&mut buffer) {
            Ok(0) => return None,
            Ok(read) => read,
            Err(_) => return None,
        };
        request.extend_from_slice(&buffer[..read]);
        if expected.is_none() {
            if let Some(end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&request[..end]);
                let length = headers.lines().find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                });
                expected = Some(end + 4 + length.unwrap_or(0));
            }
        }
        if expected.is_some_and(|length| request.len() >= length) {
            return Some(request);
        }
    }
}

#[test]
fn loopback_downloader_connections_bypass_system_proxy() {
    if std::env::var_os(CHILD_ENV).is_some() {
        let qb = Server::new("200 OK", "Set-Cookie: SID=test; HttpOnly\r\n", "Ok.");
        let tr = Server::new(
            "200 OK",
            "Content-Type: application/json\r\n",
            r#"{"result":"success","arguments":{"rpc-version":16}}"#,
        );
        let qb_result = QbitDownloader::connect(QbitConfig {
            url: qb.url.clone(),
            username: "admin".into(),
            password: "pass".into(),
            category: None,
            path_maps: vec![],
        });
        assert!(
            qb_result.is_ok(),
            "loopback qBittorrent connection: {:?}",
            qb_result.as_ref().err()
        );
        let tr_result = TransmissionDownloader::connect(TransmissionConfig {
            url: tr.url.clone(),
            username: None,
            password: None,
            path_maps: vec![],
        });
        assert!(
            tr_result.is_ok(),
            "loopback Transmission connection: {:?}",
            tr_result.as_ref().err()
        );
        // close() 会 join worker，因此这里读到的是最终计数（无竞态）。
        let qb_served = qb.close();
        let tr_served = tr.close();
        assert!(
            qb_served >= 1,
            "loopback qBittorrent server never read a complete request"
        );
        assert!(
            tr_served >= 1,
            "loopback Transmission server never read a complete request"
        );
        return;
    }

    let proxy = ProxyServer::new();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", TEST_NAME, "--nocapture"])
        .env(CHILD_ENV, "1")
        .env("HTTP_PROXY", &proxy.url)
        .env("HTTPS_PROXY", &proxy.url)
        .env("ALL_PROXY", &proxy.url)
        .env("http_proxy", &proxy.url)
        .env("https_proxy", &proxy.url)
        .env("all_proxy", &proxy.url)
        .env_remove("NO_PROXY")
        .env_remove("no_proxy")
        .output()
        .unwrap();
    let proxy_hits = proxy.close();

    assert!(
        output.status.success(),
        "local downloader connection failed with a system proxy set:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        proxy_hits, 0,
        "loopback downloader traffic reached the proxy"
    );
}
