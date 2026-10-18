//! Stateful Transmission RPC mock (rpc-version configurable; < 16 = no labels).

use std::io::Write;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;

use parking_lot::Mutex;
use serde_json::{Value, json};

use super::mock_qbit::read_request;

#[derive(Clone, Debug)]
pub struct TrTask {
    pub hash: String,
    pub name: String,
    pub filename: String,
    pub percent: f64,
    /// (name relative to download dir, length, bytes completed, wanted)
    pub files: Vec<(String, u64, u64, bool)>,
}

pub struct MockTransmission {
    pub url: String,
    pub download_dir: PathBuf,
    pub rpc_version: u64,
    pub tasks: Mutex<Vec<TrTask>>,
    pub sets: Mutex<Vec<Value>>,
    /// F-1: file names (metadata) given to the next added task.
    pub next_files: Mutex<Vec<String>>,
    next: AtomicU64,
}

impl MockTransmission {
    pub fn start(download_dir: &Path, rpc_version: u64) -> Arc<Self> {
        std::fs::create_dir_all(download_dir).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mock = Arc::new(Self {
            url: format!("http://{}/transmission/rpc", listener.local_addr().unwrap()),
            download_dir: download_dir.to_path_buf(),
            rpc_version,
            tasks: Mutex::new(Vec::new()),
            sets: Mutex::new(Vec::new()),
            next_files: Mutex::new(Vec::new()),
            next: AtomicU64::new(1),
        });
        let server = mock.clone();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let server = server.clone();
                thread::spawn(move || {
                    let req = read_request(&mut stream);
                    let body = req.split("\r\n\r\n").nth(1).unwrap_or("{}");
                    let call: Value = serde_json::from_str(body).unwrap_or(json!({}));
                    let reply = server.handle(&call).to_string();
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{reply}",
                        reply.len()
                    );
                    let _ = stream.write_all(response.as_bytes());
                });
            }
        });
        mock
    }

    pub fn config(&self) -> downloader::TransmissionConfig {
        downloader::TransmissionConfig {
            url: self.url.clone(),
            username: None,
            password: None,
            path_maps: Vec::new(),
        }
    }

    /// Finish task `index`: write `files` (relative to `<dir>/<name>/`) to disk.
    pub fn complete(&self, index: usize, name: &str, files: &[(&str, &[u8])]) {
        let mut tasks = self.tasks.lock();
        let task = &mut tasks[index];
        task.name = name.into();
        task.percent = 1.0;
        task.files.clear();
        for (rel, content) in files {
            let rel = format!("{name}/{rel}");
            let path = self.download_dir.join(&rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, content).unwrap();
            let len = content.len() as u64;
            task.files.push((rel, len, len, true));
        }
    }

    fn handle(&self, call: &Value) -> Value {
        let args = &call["arguments"];
        match call["method"].as_str().unwrap_or_default() {
            "session-get" => {
                json!({"result": "success", "arguments": {"rpc-version": self.rpc_version}})
            }
            "torrent-add" => {
                let n = self.next.fetch_add(1, Ordering::SeqCst);
                let hash = format!("{n:040x}");
                self.tasks.lock().push(TrTask {
                    hash: hash.clone(),
                    name: format!("task-{n}"),
                    filename: args["filename"].as_str().unwrap_or_default().into(),
                    percent: 0.0,
                    files: std::mem::take(&mut *self.next_files.lock())
                        .into_iter()
                        .map(|name| (name, 1, 0, true))
                        .collect(),
                });
                json!({"result": "success", "arguments": {"torrent-added": {"hashString": hash, "id": n}}})
            }
            "torrent-set" => {
                self.sets.lock().push(args.clone());
                json!({"result": "success", "arguments": {}})
            }
            "torrent-get" => {
                let tasks = self.tasks.lock();
                let rows: Vec<Value> = tasks
                    .iter()
                    .enumerate()
                    .map(|(i, t)| {
                        let size: u64 = t.files.iter().map(|f| f.1).sum();
                        json!({
                            "id": i + 1, "name": t.name, "hashString": t.hash,
                            "totalSize": size, "sizeWhenDone": size, "percentDone": t.percent,
                            "downloadDir": self.download_dir.display().to_string(),
                            "rateDownload": 0, "rateUpload": 0, "status": 4, "error": 0,
                            "downloadedEver": 0, "uploadedEver": 0,
                            "files": t.files.iter().map(|f| json!({"name": f.0, "length": f.1, "bytesCompleted": f.2})).collect::<Vec<_>>(),
                            "fileStats": t.files.iter().map(|f| json!({"wanted": f.3})).collect::<Vec<_>>(),
                        })
                    })
                    .collect();
                json!({"result": "success", "arguments": {"torrents": rows}})
            }
            _ => json!({"result": "success", "arguments": {}}),
        }
    }
}
