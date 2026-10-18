//! Stateful fake qBittorrent WebUI (v2 API subset) for end-to-end flow tests.
//!
//! Binds `127.0.0.1:0`; tasks are created by `torrents/add`, and tests drive
//! the download lifecycle with [`MockQbit::complete`] which writes real files
//! under the mock save path so Transfer can import them.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::thread;

use parking_lot::Mutex;
use serde_json::json;

#[derive(Clone, Debug)]
pub struct Task {
    pub hash: String,
    pub name: String,
    pub tags: Vec<String>,
    pub progress: f64,
    pub save_path: String,
    pub files: Vec<(String, f64)>,
    pub size: u64,
    pub state: String,
    /// `urls` field or `file` when a .torrent part was uploaded.
    pub source: String,
}

pub struct MockQbit {
    pub url: String,
    pub save_root: PathBuf,
    pub tasks: Mutex<Vec<Task>>,
    pub adds: Mutex<Vec<String>>,
    pub deletes: Mutex<Vec<String>>,
    pub resumes: Mutex<Vec<String>>,
    pub logins: AtomicUsize,
    pub info_calls: AtomicUsize,
    /// D-L2: `torrents/info` calls without a `tag`/`hashes` filter.
    pub unfiltered_info_calls: AtomicUsize,
    /// Reject every authenticated request with 403 while > 0 (decrements per request).
    pub forbid_next: AtomicUsize,
    /// When set, login answers like a wrong password (200 `Fails.`, no cookie).
    pub reject_login: std::sync::atomic::AtomicBool,
    /// Bytes served at `GET /download.php` (acts as the PT site's .torrent).
    pub torrent_file: Mutex<Option<String>>,
    pub add_tags: Mutex<Vec<String>>,
    /// F-1: file names (metadata) given to the next added task, progress 0.
    pub next_files: Mutex<Vec<String>>,
    /// F-1: raw `torrents/filePrio` requests.
    pub file_prios: Mutex<Vec<String>>,
    /// (hash, file index) set to priority 0.
    pub skipped: Mutex<Vec<(String, usize)>>,
    next: AtomicU64,
}

impl MockQbit {
    pub fn start(save_root: &Path) -> Arc<Self> {
        std::fs::create_dir_all(save_root).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let mock = Arc::new(Self {
            url: format!("http://{addr}"),
            save_root: save_root.to_path_buf(),
            tasks: Mutex::new(Vec::new()),
            adds: Mutex::new(Vec::new()),
            deletes: Mutex::new(Vec::new()),
            logins: AtomicUsize::new(0),
            info_calls: AtomicUsize::new(0),
            unfiltered_info_calls: AtomicUsize::new(0),
            forbid_next: AtomicUsize::new(0),
            reject_login: std::sync::atomic::AtomicBool::new(false),
            torrent_file: Mutex::new(None),
            resumes: Mutex::new(Vec::new()),
            add_tags: Mutex::new(Vec::new()),
            next_files: Mutex::new(Vec::new()),
            file_prios: Mutex::new(Vec::new()),
            skipped: Mutex::new(Vec::new()),
            next: AtomicU64::new(1),
        });
        let server = mock.clone();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let server = server.clone();
                thread::spawn(move || {
                    let req = read_request(&mut stream);
                    let (status, body, extra) = server.route(&req);
                    let response = format!(
                        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: text/plain\r\nConnection: close\r\n{extra}\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(response.as_bytes());
                });
            }
        });
        mock
    }

    pub fn config(&self) -> downloader::QbitConfig {
        downloader::QbitConfig {
            url: self.url.clone(),
            username: "admin".into(),
            password: "pass".into(),
            category: Some("crawler-media".into()),
            path_maps: vec![],
        }
    }

    pub fn task_count(&self) -> usize {
        self.tasks.lock().len()
    }

    pub fn add_count(&self) -> usize {
        self.adds.lock().len()
    }

    /// Insert a task directly (e.g. one the user added by hand).
    pub fn inject(&self, task: Task) {
        self.tasks.lock().push(task);
    }

    pub fn remove_all(&self) {
        self.tasks.lock().clear();
    }

    pub fn set_progress(&self, index: usize, progress: f64) {
        let mut tasks = self.tasks.lock();
        tasks[index].progress = progress;
        tasks[index].state = if progress >= 1.0 {
            "uploading".into()
        } else {
            "downloading".into()
        };
    }

    /// D-L1: set one file's progress (and the task's average progress).
    pub fn set_file_progress(&self, index: usize, file: usize, progress: f64) {
        let mut tasks = self.tasks.lock();
        let task = &mut tasks[index];
        task.files[file].1 = progress;
        let total: f64 = task.files.iter().map(|(_, p)| p).sum();
        task.progress = total / task.files.len() as f64;
        task.state = if task.progress >= 1.0 {
            "uploading"
        } else {
            "downloading"
        }
        .into();
    }

    /// Finish task `index`: rename it, write the given files (relative to the
    /// task folder) with `content`, and mark everything 100%.
    pub fn complete(&self, index: usize, name: &str, files: &[(&str, &[u8])]) -> Vec<PathBuf> {
        let mut tasks = self.tasks.lock();
        let task = &mut tasks[index];
        task.name = name.to_string();
        task.progress = 1.0;
        task.state = "uploading".into();
        task.files.clear();
        let mut written = Vec::new();
        let mut size = 0;
        for (rel, content) in files {
            let rel = format!("{name}/{rel}");
            let path = Path::new(&task.save_path).join(&rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, content).unwrap();
            size += content.len() as u64;
            task.files.push((rel, 1.0));
            written.push(path);
        }
        task.size = size;
        written
    }

    fn route(&self, req: &str) -> (&'static str, String, &'static str) {
        if req.starts_with("POST /api/v2/auth/login") {
            self.logins.fetch_add(1, Ordering::SeqCst);
            if self.reject_login.load(Ordering::SeqCst) {
                return ("200 OK", "Fails.".into(), "");
            }
            return (
                "200 OK",
                "Ok.".into(),
                "Set-Cookie: SID=e2e-sid; HttpOnly\r\n",
            );
        }
        if req.starts_with("GET /download.php") {
            return match self.torrent_file.lock().clone() {
                Some(bytes) => ("200 OK", bytes, ""),
                None => ("404 Not Found", "no".into(), ""),
            };
        }
        if self
            .forbid_next
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
        {
            return ("403 Forbidden", "Forbidden".into(), "");
        }
        if req.starts_with("POST /api/v2/torrents/add ")
            || req.starts_with("POST /api/v2/torrents/add?")
        {
            self.adds.lock().push(req.to_string());
            let tags = multipart_field(req, "tags").unwrap_or_default();
            let source = if req.contains("name=\"torrents\"") {
                "file".to_string()
            } else {
                multipart_field(req, "urls").unwrap_or_default()
            };
            let n = self.next.fetch_add(1, Ordering::SeqCst);
            self.tasks.lock().push(Task {
                hash: format!("{n:040x}"),
                name: format!("task-{n}"),
                tags: tags
                    .split(',')
                    .map(|t| t.trim().to_string())
                    .filter(|t| !t.is_empty())
                    .collect(),
                progress: 0.0,
                save_path: self.save_root.display().to_string(),
                files: std::mem::take(&mut *self.next_files.lock())
                    .into_iter()
                    .map(|name| (name, 0.0))
                    .collect(),
                size: 0,
                state: "downloading".into(),
                source,
            });
            return ("200 OK", "Ok.".into(), "");
        }
        if req.contains("/api/v2/torrents/addTags") {
            self.add_tags.lock().push(req.to_string());
            let hashes = form_field(req, "hashes").unwrap_or_default();
            let tags = form_field(req, "tags")
                .unwrap_or_default()
                .replace("%2C", ",");
            for task in self.tasks.lock().iter_mut() {
                if hashes
                    .split('|')
                    .any(|h| h.eq_ignore_ascii_case(&task.hash))
                {
                    for tag in tags.split(',').map(str::trim).filter(|t| !t.is_empty()) {
                        if !task.tags.iter().any(|t| t == tag) {
                            task.tags.push(tag.to_string());
                        }
                    }
                }
            }
            return ("200 OK", "Ok.".into(), "");
        }
        if req.contains("/api/v2/torrents/info") {
            self.info_calls.fetch_add(1, Ordering::SeqCst);
            let tag = query_param(req, "tag").map(|t| percent_decode(&t));
            let hashes = query_param(req, "hashes");
            if tag.is_none() && hashes.is_none() {
                self.unfiltered_info_calls.fetch_add(1, Ordering::SeqCst);
            }
            let tasks = self.tasks.lock();
            let body: Vec<_> = tasks
                .iter()
                .filter(|t| {
                    tag.as_ref()
                        .is_none_or(|tag| t.tags.iter().any(|x| x == tag))
                })
                .filter(|t| {
                    hashes
                        .as_ref()
                        .is_none_or(|hs| hs.split('|').any(|h| h.eq_ignore_ascii_case(&t.hash)))
                })
                .map(|t| {
                    json!({
                        "hash": t.hash, "name": t.name, "progress": t.progress,
                        "save_path": t.save_path, "tags": t.tags.join(","),
                        "size": t.size, "uploaded": 0, "category": "crawler-media",
                        "state": t.state, "downloaded": 0, "dlspeed": 0, "upspeed": 0,
                    })
                })
                .collect();
            return ("200 OK", serde_json::to_string(&body).unwrap(), "");
        }
        if req.contains("/api/v2/torrents/files") {
            let hash = query_param(req, "hash").unwrap_or_default();
            let tasks = self.tasks.lock();
            let skipped = self.skipped.lock();
            let body: Vec<_> = tasks
                .iter()
                .filter(|t| t.hash.eq_ignore_ascii_case(&hash))
                .flat_map(|t| {
                    t.files.iter().enumerate().map(|(i, (name, p))| {
                        let off = skipped.iter().any(|(h, j)| h.eq_ignore_ascii_case(&t.hash) && *j == i);
                        json!({"index": i, "name": name, "progress": p, "priority": if off { 0 } else { 1 }})
                    })
                })
                .collect();
            return ("200 OK", serde_json::to_string(&body).unwrap(), "");
        }
        if req.contains("/api/v2/torrents/filePrio") {
            self.file_prios.lock().push(req.to_string());
            let hash = form_field(req, "hash").unwrap_or_default();
            let ids = form_field(req, "id")
                .unwrap_or_default()
                .replace("%7C", "|");
            for id in ids.split('|').filter_map(|i| i.parse::<usize>().ok()) {
                self.skipped.lock().push((hash.to_lowercase(), id));
            }
            return ("200 OK", "".into(), "");
        }
        if req.contains("/api/v2/torrents/resume") || req.contains("/api/v2/torrents/start") {
            self.resumes.lock().push(req.to_string());
            if let Some(hashes) = form_field(req, "hashes") {
                for task in self.tasks.lock().iter_mut() {
                    if hashes
                        .split('|')
                        .any(|h| h.eq_ignore_ascii_case(&task.hash))
                    {
                        task.state = "downloading".into();
                    }
                }
            }
            return ("200 OK", "Ok.".into(), "");
        }
        if req.contains("/api/v2/torrents/delete") {
            self.deletes.lock().push(req.to_string());
            if let Some(hashes) = form_field(req, "hashes") {
                self.tasks
                    .lock()
                    .retain(|t| !hashes.split('|').any(|h| h.eq_ignore_ascii_case(&t.hash)));
            }
            return ("200 OK", "Ok.".into(), "");
        }
        if req.contains("/api/v2/app/preferences") {
            return ("200 OK", r#"{"dl_limit":0,"up_limit":0}"#.into(), "");
        }
        ("200 OK", "Ok.".into(), "")
    }
}

pub fn read_request(stream: &mut TcpStream) -> String {
    let mut request = Vec::new();
    let mut expected = None;
    loop {
        let mut buf = [0u8; 8192];
        let n = stream.read(&mut buf).unwrap_or(0);
        if n == 0 {
            break;
        }
        request.extend_from_slice(&buf[..n]);
        if expected.is_none() {
            if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&request[..end]);
                let len = headers.lines().find_map(|line| {
                    let (k, v) = line.split_once(':')?;
                    k.eq_ignore_ascii_case("content-length")
                        .then(|| v.trim().parse::<usize>().ok())
                        .flatten()
                });
                expected = Some(end + 4 + len.unwrap_or(0));
            }
        }
        if expected.is_some_and(|len| request.len() >= len) {
            break;
        }
    }
    String::from_utf8_lossy(&request).into_owned()
}

fn multipart_field(req: &str, name: &str) -> Option<String> {
    let marker = format!("name=\"{name}\"");
    let start = req.find(&marker)?;
    let rest = &req[start..];
    let body_start = rest.find("\r\n\r\n")? + 4;
    let body = &rest[body_start..];
    let end = body.find("\r\n--").unwrap_or(body.len());
    Some(body[..end].to_string())
}

fn query_param(req: &str, key: &str) -> Option<String> {
    let line = req.lines().next()?;
    let path = line.split_whitespace().nth(1)?;
    let query = path.split_once('?')?.1;
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key).then(|| v.to_string())
    })
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&value[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(if bytes[i] == b'+' { b' ' } else { bytes[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn form_field(req: &str, key: &str) -> Option<String> {
    let body = req.split_once("\r\n\r\n")?.1;
    body.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key).then(|| v.replace("%7C", "|"))
    })
}
