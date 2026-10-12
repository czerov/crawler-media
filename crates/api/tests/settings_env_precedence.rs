//! DB9：env 固定的 key 与设置页写入的优先级。
//!
//! 策略是「env 固定该 key」：启动时 env 值照常 seed 进 settings KV；此后
//! `PUT /settings/metadata`、`PUT /settings/proxy` 在目标 key 被 env 固定时
//! 必须返回 409，而不是接受一个重启就会被覆盖的写入。
//!
//! 这些用例通过**隔离进程**运行真实 `crawler-media` 二进制：既不 `unsafe`
//! 修改测试进程的全局 env，也覆盖「启动 → UI 保存 → 重启」的完整场景。
//! 同时覆盖 DB10 的代理 env 语义（空串不算 override、`_FILE` 在兜底路径生效）。

use std::fs::File;
use std::io::Read;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use api::{Store, settings_keys};
use serde_json::{Value, json};

const TOKEN: &str = "env-precedence-cli-token";
const ADMIN_PASSWORD: &str = "env-precedence-admin-pass";
const UI_TMDB_KEY: &str = "ui-key-abcdefgh";
const ENV_TMDB_KEY: &str = "env-key-12345678";

/// 真实二进制 + 临时数据目录，Drop 时杀掉子进程。
struct Server {
    child: Child,
    port: u16,
    agent: ureq::Agent,
    stderr_path: PathBuf,
}

impl Server {
    fn start(data_dir: &Path, env: &[(&str, &str)]) -> Self {
        let port = free_port();
        std::fs::create_dir_all(data_dir).expect("create data dir");
        let stdout_path = data_dir.join("server-stdout.log");
        let stderr_path = data_dir.join("server-stderr.log");
        let stdout = File::create(&stdout_path).expect("server stdout log");
        let stderr = File::create(&stderr_path).expect("server stderr log");
        let mut command = Command::new(env!("CARGO_BIN_EXE_crawler-media"));
        command
            .arg("serve")
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", std::env::var("HOME").unwrap_or_default())
            .env("CRAWLER_MEDIA_DATA", data_dir)
            .env("CRAWLER_MEDIA_TOKEN", TOKEN)
            .env("CRAWLER_MEDIA_ADMIN_PASSWORD", ADMIN_PASSWORD)
            .env("CRAWLER_MEDIA_LISTEN", format!("127.0.0.1:{port}"))
            // stdout 是 tracing 的默认 writer；写文件而不是管道，既不阻塞也能断言日志。
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr));
        for (key, value) in env {
            command.env(key, value);
        }
        let child = command.spawn().expect("spawn crawler-media serve");
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(10)))
            .build()
            .new_agent();
        let mut server = Self {
            child,
            port,
            agent,
            stderr_path,
        };
        server.wait_ready();
        server
    }

    fn wait_ready(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(status) = self.child.try_wait().expect("try_wait") {
                panic!(
                    "crawler-media 启动即退出（{status}）：{}",
                    self.drain_stderr()
                );
            }
            if self.try_ready() {
                return;
            }
            if Instant::now() >= deadline {
                panic!("等待 crawler-media 就绪超时：{}", self.drain_stderr());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// 就绪探测必须容忍连接被拒（子进程还在启动），不能 panic。
    fn try_ready(&self) -> bool {
        let url = format!("http://127.0.0.1:{}/api/v1/settings/metadata", self.port);
        match self
            .agent
            .get(&url)
            .header("Authorization", &format!("Bearer {TOKEN}"))
            .call()
        {
            Ok(response) => response.status().as_u16() == 200,
            Err(_) => false,
        }
    }

    fn drain_stderr(&self) -> String {
        std::fs::read_to_string(&self.stderr_path).unwrap_or_default()
    }

    fn request(&self, method: &str, path: &str, body: Option<&str>) -> (u16, String) {
        let url = format!("http://127.0.0.1:{}{path}", self.port);
        let auth = format!("Bearer {TOKEN}");
        let response = match (method, body) {
            ("GET", None) => self.agent.get(&url).header("Authorization", &auth).call(),
            ("PUT", Some(body)) => self
                .agent
                .put(&url)
                .header("Authorization", &auth)
                .header("Content-Type", "application/json")
                .send(body),
            (method, _) => panic!("unsupported {method} request"),
        }
        .expect("request crawler-media");
        let status = response.status().as_u16();
        let text = response
            .into_body()
            .read_to_string()
            .unwrap_or_else(|_| String::new());
        (status, text)
    }

    fn get_json(&self, path: &str) -> Value {
        let (status, body) = self.request("GET", path, None);
        assert_eq!(status, 200, "GET {path} -> {body}");
        serde_json::from_str(&body).unwrap_or_else(|e| panic!("GET {path} 非 JSON: {e}: {body}"))
    }

    fn put_json(&self, path: &str, body: Value) -> (u16, Value) {
        let (status, text) = self.request("PUT", path, Some(&body.to_string()));
        let parsed = serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("PUT {path} 非 JSON: {e}: {text}"));
        (status, parsed)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    listener.local_addr().expect("local_addr").port()
}

fn stored(data_dir: &Path, key: &str) -> Option<String> {
    Store::open(data_dir)
        .expect("open store")
        .get_setting(key)
        .expect("get_setting")
}

fn data_dir(root: &tempfile::TempDir) -> PathBuf {
    root.path().join("data")
}

/// 子进程的 stdout(tracing) + stderr(裸 eprintln) 全文。
fn read_logs(data_dir: &Path) -> String {
    let mut logs = String::new();
    for name in ["server-stdout.log", "server-stderr.log"] {
        logs.push_str(&std::fs::read_to_string(data_dir.join(name)).unwrap_or_default());
    }
    logs
}

fn write_secret_file(root: &tempfile::TempDir, name: &str, value: &str) -> String {
    let path = root.path().join(name);
    std::fs::write(&path, value).expect("write secret file");
    path.to_string_lossy().into_owned()
}

#[test]
fn env_unset_ui_write_survives_restart() {
    let root = tempfile::tempdir().unwrap();
    let data = data_dir(&root);

    {
        let server = Server::start(&data, &[]);
        let before = server.get_json("/api/v1/settings/metadata");
        assert_eq!(
            before["data"]["tmdb"]["env_override"], false,
            "env 未设时 env_override 必须为 false: {before}"
        );

        let (status, body) = server.put_json(
            "/api/v1/settings/metadata",
            json!({ "tmdb_api_key": UI_TMDB_KEY }),
        );
        assert_eq!(status, 200, "env 未设时 UI 写入必须被接受: {body}");

        let after = server.get_json("/api/v1/settings/metadata");
        assert_eq!(after["data"]["tmdb"]["configured"], true, "{after}");
        assert_eq!(after["data"]["tmdb"]["env_override"], false, "{after}");
    }

    assert_eq!(
        stored(&data, settings_keys::TMDB_API_KEY).as_deref(),
        Some(UI_TMDB_KEY),
        "UI 写入必须落盘"
    );

    // 重启：env 仍未设 → 保留 UI 值。
    let server = Server::start(&data, &[]);
    let restarted = server.get_json("/api/v1/settings/metadata");
    assert_eq!(restarted["data"]["tmdb"]["configured"], true, "{restarted}");
    assert_eq!(
        restarted["data"]["tmdb"]["env_override"], false,
        "{restarted}"
    );
    drop(server);
    assert_eq!(
        stored(&data, settings_keys::TMDB_API_KEY).as_deref(),
        Some(UI_TMDB_KEY),
        "重启后 UI 值必须仍然生效"
    );
}

#[test]
fn env_pinned_tmdb_rejects_ui_write_and_keeps_env_value() {
    let root = tempfile::tempdir().unwrap();
    let data = data_dir(&root);
    let env = [("CRAWLER_MEDIA_TMDB_KEY", ENV_TMDB_KEY)];

    {
        let server = Server::start(&data, &env);
        let before = server.get_json("/api/v1/settings/metadata");
        assert_eq!(
            before["data"]["tmdb"]["env_override"], true,
            "env 已设时 env_override 必须为 true: {before}"
        );
        assert_eq!(before["data"]["tmdb"]["configured"], true, "{before}");

        let (status, body) = server.put_json(
            "/api/v1/settings/metadata",
            json!({ "tmdb_api_key": UI_TMDB_KEY }),
        );
        assert_eq!(status, 409, "env 固定 TMDB key 时写入必须 409: {body}");
        assert_eq!(body["ok"], false, "{body}");
        assert_eq!(body["error"]["code"], "settings.env_pinned", "{body}");
    }

    // 被拒绝的写入不得落盘。
    assert_eq!(
        stored(&data, settings_keys::TMDB_API_KEY).as_deref(),
        Some(ENV_TMDB_KEY),
        "409 之后 store 必须仍是 env 值"
    );

    // 重启：env 仍然固定该 key。
    let server = Server::start(&data, &env);
    let restarted = server.get_json("/api/v1/settings/metadata");
    assert_eq!(
        restarted["data"]["tmdb"]["env_override"], true,
        "{restarted}"
    );
    assert_eq!(restarted["data"]["tmdb"]["configured"], true, "{restarted}");
    drop(server);
    assert_eq!(
        stored(&data, settings_keys::TMDB_API_KEY).as_deref(),
        Some(ENV_TMDB_KEY),
        "重启后必须仍是 env 值"
    );
}

#[test]
fn env_pinned_proxy_rejects_ui_write() {
    let root = tempfile::tempdir().unwrap();
    let data = data_dir(&root);
    let env_url = "socks5://127.0.0.1:1080";
    let env = [("CRAWLER_MEDIA_METADATA_PROXY", env_url)];

    {
        let server = Server::start(&data, &env);
        let before = server.get_json("/api/v1/settings/proxy");
        assert_eq!(before["data"]["env_override"], true, "{before}");

        let (status, body) = server.put_json(
            "/api/v1/settings/proxy",
            json!({ "proxy_url": "http://127.0.0.1:8080" }),
        );
        assert_eq!(status, 409, "env 固定代理 URL 时写入必须 409: {body}");
        assert_eq!(body["error"]["code"], "settings.env_pinned", "{body}");
    }

    assert_eq!(
        stored(&data, settings_keys::PROXY_METADATA).as_deref(),
        Some(env_url),
        "409 之后 store 必须仍是 env 值"
    );
}

#[test]
fn empty_proxy_env_is_not_an_override() {
    let root = tempfile::tempdir().unwrap();
    let data = data_dir(&root);

    let server = Server::start(&data, &[("CRAWLER_MEDIA_METADATA_PROXY", "")]);
    let proxy = server.get_json("/api/v1/settings/proxy");
    assert_eq!(
        proxy["data"]["env_override"], false,
        "空串 env 不算 override（与运行时非空规则一致）: {proxy}"
    );
    assert!(
        proxy["data"]["active_proxy"].is_null(),
        "空串代理不得生效: {proxy}"
    );

    // 空串同样不是 pin：UI 写入必须被接受。
    let (status, body) =
        server.put_json("/api/v1/settings/proxy", json!({ "proxy_url": "http://127.0.0.1:8080" }));
    assert_eq!(status, 200, "空串 env 不得固定该 key: {body}");
}

#[test]
fn proxy_secret_file_env_is_used_by_fallback_path() {
    let root = tempfile::tempdir().unwrap();
    let data = data_dir(&root);
    let user_file = write_secret_file(&root, "proxy_user", "alice\n");

    // 只给 `_USER_FILE`：config 不会把它 seed 进 store（store 里没有用户名），
    // 因此这条路径只能由 http_agent 的 env 兜底解析命中。
    let env = [
        ("CRAWLER_MEDIA_METADATA_PROXY", "socks5://127.0.0.1:1080"),
        ("CRAWLER_MEDIA_METADATA_PROXY_USER_FILE", user_file.as_str()),
    ];
    let server = Server::start(&data, &env);
    let proxy = server.get_json("/api/v1/settings/proxy");
    assert_eq!(proxy["data"]["env_override"], true, "{proxy}");
    assert_eq!(
        proxy["data"]["active_proxy"], "socks5://alice@127.0.0.1:1080",
        "兜底路径必须解析 _FILE 变体: {proxy}"
    );
    assert_eq!(
        proxy["data"]["username"], "",
        "store 未 seed 时不应假装已保存: {proxy}"
    );
}

#[test]
fn env_whitelist_warning_goes_through_tracing() {
    let root = tempfile::tempdir().unwrap();
    let data = data_dir(&root);
    // `CRAWLER_MEDIA_FFMPEG_LOG_LEVEL` 是真实在用的变量（marker crate），
    // `CRAWLER_MEDIA_TMDB_KET` 是 typo：前者不得打假 warning，后者必须被记录。
    let server = Server::start(
        &data,
        &[
            ("CRAWLER_MEDIA_FFMPEG_LOG_LEVEL", "info"),
            ("CRAWLER_MEDIA_TMDB_KET", "typo"),
        ],
    );
    drop(server);

    let logs = read_logs(&data);
    assert!(
        logs.contains("CRAWLER_MEDIA_TMDB_KET"),
        "未识别的 env 变量必须经 tracing 记录: {logs}"
    );
    assert!(
        !logs.contains("CRAWLER_MEDIA_FFMPEG_LOG_LEVEL"),
        "真实在用的变量不得产生假 warning: {logs}"
    );
}
