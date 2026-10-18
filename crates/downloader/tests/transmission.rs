use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;

use parking_lot::Mutex;

use domain::{DownloaderId, SiteId, Torrent};
use downloader::{
    Downloader, DownloaderSet, MemoryDownloader, TransmissionConfig, TransmissionDownloader,
};

struct Capture {
    methods: Mutex<Vec<String>>,
    requests: Mutex<Vec<String>>,
    injected_torrents: Mutex<Option<String>>,
    injected_add: Mutex<Option<String>>,
}

impl Capture {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            methods: Mutex::new(Vec::new()),
            requests: Mutex::new(Vec::new()),
            injected_torrents: Mutex::new(None),
            injected_add: Mutex::new(None),
        })
    }
}

fn torrent() -> Torrent {
    Torrent {
        site_id: SiteId::new(),
        title: "The.Matrix.1999.2160p.BluRay.x265-GROUP".into(),
        enclosure: "https://pt.example/download.php?id=1".into(),
        size_bytes: Some(1),
        seeders: Some(1),
        free: true,
        hr: false,
        imdb_id: None,
        id: None,
        leechers: None,
        snatched: None,
        upload_time: None,
        detail_url: None,
        category: None,
        poster_url: None,
    }
}

fn read_request(stream: &mut TcpStream) -> String {
    let mut request = Vec::new();
    let mut expected = None;
    loop {
        let mut buf = [0u8; 4096];
        let n = stream.read(&mut buf).unwrap_or(0);
        if n == 0 {
            break;
        }
        request.extend_from_slice(&buf[..n]);
        if expected.is_none() {
            if let Some(header_end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&request[..header_end]);
                let content_length = headers.lines().find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                });
                expected = Some(header_end + 4 + content_length.unwrap_or(0));
            }
        }
        if expected.is_some_and(|length| request.len() >= length) {
            break;
        }
    }
    String::from_utf8_lossy(&request).into_owned()
}

fn serve(capture: Arc<Capture>, save_path: String) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    thread::spawn(move || {
        for incoming in listener.incoming() {
            let mut stream = incoming.unwrap();
            let req = read_request(&mut stream);
            let method = if req.contains("session-get") {
                "session-get"
            } else if req.contains("torrent-add") {
                "torrent-add"
            } else if req.contains("torrent-get") {
                "torrent-get"
            } else if req.contains("torrent-remove") {
                "torrent-remove"
            } else if req.contains("torrent-set") {
                "torrent-set"
            } else {
                "unknown"
            };
            capture.methods.lock().push(method.into());
            if method == "torrent-add" || method == "torrent-remove" || method == "torrent-set" {
                capture.requests.lock().push(req.clone());
            }
            let body = match method {
                "session-get" => {
                    r#"{"result":"success","arguments":{"rpc-version":16}}"#.to_string()
                }
                "torrent-add" => capture
                    .injected_add
                    .lock()
                    .clone()
                    .unwrap_or_else(|| r#"{"result":"success","arguments":{}}"#.to_string()),
                "torrent-set" => r#"{"result":"success","arguments":{}}"#.to_string(),
                "torrent-get" => {
                    let injected = capture.injected_torrents.lock().clone();
                    if let Some(custom) = injected {
                        custom
                    } else {
                        format!(
                            r#"{{"result":"success","arguments":{{"torrents":[{{"id":1,"name":"The.Matrix.1999.2160p.BluRay.x265-GROUP","hashString":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","sizeWhenDone":1,"percentDone":1.0,"downloadDir":"{save_path}","labels":["crawler-media","{}"],"files":[{{"name":"matrix.mkv"}}]}}]}}}}"#,
                            downloader::ownership_tag("https://pt.example/download.php?id=1")
                        )
                    }
                }
                "torrent-remove" => r#"{"result":"success","arguments":{}}"#.to_string(),
                _ => r#"{"result":"failure"}"#.to_string(),
            };
            let response = format!(
                "HTTP/1.1 200 OK\r\nX-Transmission-Session-Id: abc\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).ok();
        }
    });
    format!("http://{addr}/transmission/rpc")
}

#[test]
fn transmission_snapshots_preserve_metrics_and_only_mark_owned_tasks() {
    let capture = Capture::new();
    *capture.injected_torrents.lock() = Some(
        serde_json::json!({
            "result": "success", "arguments": {"torrents": [
                {"name": "owned", "hashString": "abc", "totalSize": 2048,
                 "percentDone": 0.5, "rateDownload": 100, "rateUpload": 20,
                 "downloadedEver": 1024, "uploadedEver": 256, "status": 4,
                 "labels": [downloader::TASK_TAG]},
                {"name": "foreign", "hashString": "def", "totalSize": 4096,
                 "percentDone": 1.0, "status": 0, "labels": ["other"]},
                {"name": "legacy", "hashString": "ghi", "status": 6,
                 "labels": ["crawler-media-old"]}
            ]}
        })
        .to_string(),
    );
    let dl = TransmissionDownloader::connect(TransmissionConfig {
        url: serve(capture, "/downloads".into()),
        username: None,
        password: None,
        path_maps: vec![],
    })
    .unwrap();
    let rows = dl.task_snapshots().unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].tag, downloader::TASK_TAG);
    assert_eq!(rows[0].size_bytes, 2048);
    assert_eq!(rows[0].info_hash, "abc");
    assert_eq!(rows[0].progress, 0.5);
    assert_eq!((rows[0].download_speed, rows[0].upload_speed), (100, 20));
    assert_eq!(
        (rows[0].downloaded_bytes, rows[0].uploaded_bytes),
        (1024, 256)
    );
    assert_eq!(rows[0].state, "downloading");
    assert_eq!(rows[1].tag, "");
    assert_eq!(rows[1].state, "pausedUP");
    assert_eq!(rows[2].tag, "crawler-media-old");
    assert_eq!(rows[2].state, "uploading");
}

#[test]
fn transmission_snapshots_expose_per_torrent_ownership_labels() {
    let owned = downloader::ownership_tag("https://pt.example/download.php?id=7");
    let capture = Capture::new();
    *capture.injected_torrents.lock() = Some(
        serde_json::json!({
            "result": "success", "arguments": {"torrents": [
                {"name": "owned", "hashString": "abc", "status": 4,
                 "labels": [downloader::TASK_TAG, owned]},
                {"name": "generic-only", "hashString": "def", "status": 4,
                 "labels": [downloader::TASK_TAG]}
            ]}
        })
        .to_string(),
    );
    let dl = TransmissionDownloader::connect(TransmissionConfig {
        url: serve(capture, "/downloads".into()),
        username: None,
        password: None,
        path_maps: vec![],
    })
    .unwrap();
    let rows = dl.task_snapshots().unwrap();
    assert_eq!(rows[0].tag, downloader::TASK_TAG);
    assert_eq!(rows[0].owned_tags, vec![owned]);
    assert!(rows[1].owned_tags.is_empty());
}

#[test]
fn transmission_adds_enclosure_over_rpc() {
    let capture = Capture::new();
    let url = serve(capture.clone(), "/downloads".into());
    let dl = TransmissionDownloader::connect(TransmissionConfig {
        url,
        username: None,
        password: None,
        path_maps: vec![],
    })
    .unwrap();
    dl.add(&torrent()).unwrap();
    let methods = capture.methods.lock().clone();
    assert!(methods.contains(&"session-get".to_string()));
    assert!(methods.contains(&"torrent-add".to_string()));
}

#[test]
fn transmission_completed_files_from_rpc() {
    let capture = Capture::new();
    let url = serve(capture, "/downloads".into());
    let dl = TransmissionDownloader::connect(TransmissionConfig {
        url,
        username: None,
        password: None,
        path_maps: vec![],
    })
    .unwrap();
    let files = dl.completed_files(&torrent()).unwrap();
    assert_eq!(files[0], std::path::Path::new("/downloads/matrix.mkv"));
}

#[test]
fn same_name_different_hash_is_not_collected_or_removed() {
    let capture = Capture::new();
    let url = serve(capture.clone(), "/downloads".into());
    let dl = TransmissionDownloader::connect(TransmissionConfig {
        url,
        username: None,
        password: None,
        path_maps: vec![],
    })
    .unwrap();
    *capture.injected_torrents.lock() = Some(
        r#"{"result":"success","arguments":{"torrents":[{
            "id":1,
            "name":"The.Matrix.1999.2160p.BluRay.x265-GROUP",
            "hashString":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "sizeWhenDone":1,
            "percentDone":1.0,
            "downloadDir":"/downloads",
            "labels":["crawler-media"],
            "files":[{"name":"matrix.mkv"}]
        }]}}"#
            .into(),
    );
    let mut target = torrent();
    target.enclosure = "magnet:?xt=urn:btih:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into();
    assert!(dl.completed_files(&target).unwrap().is_empty());
    assert!(
        dl.remove(&target, true).is_ok(),
        "目标 magnet 不在客户端时应安全 no-op"
    );
    assert!(
        !capture
            .requests
            .lock()
            .iter()
            .any(|req| req.contains("torrent-remove")),
        "同名不同 hash 绝不能触发 Transmission 删除"
    );
}

#[test]
fn completed_files_rejects_known_size_conflict() {
    let capture = Capture::new();
    let url = serve(capture.clone(), "/downloads".into());
    let dl = TransmissionDownloader::connect(TransmissionConfig {
        url,
        username: None,
        password: None,
        path_maps: vec![],
    })
    .unwrap();

    // 客户端内任务大小为 2000，目标已知大小为 1000
    *capture.injected_torrents.lock() = Some(r#"{"result":"success","arguments":{"torrents":[
        {"id":1,"name":"The.Matrix.1999.2160p.BluRay.x265-GROUP","sizeWhenDone":2000,"percentDone":1.0,"downloadDir":"/downloads","files":[{"name":"matrix.mkv"}]}
    ]}}"#.to_string());

    let mut target = torrent();
    target.size_bytes = Some(1000);

    let files = dl.completed_files(&target).unwrap();
    assert!(files.is_empty(), "已知体积产生冲突的任务绝不能被收集文件");
}

#[test]
fn completed_files_rejects_ambiguous_identity() {
    let capture = Capture::new();
    let url = serve(capture.clone(), "/downloads".into());
    let dl = TransmissionDownloader::connect(TransmissionConfig {
        url,
        username: None,
        password: None,
        path_maps: vec![],
    })
    .unwrap();

    // 两个相同同名同体积任务，无法唯一消歧
    *capture.injected_torrents.lock() = Some(r#"{"result":"success","arguments":{"torrents":[
        {"id":1,"name":"The.Matrix.1999.2160p.BluRay.x265-GROUP","sizeWhenDone":1000,"percentDone":1.0,"downloadDir":"/downloads","files":[{"name":"matrix1.mkv"}]},
        {"id":2,"name":"The.Matrix.1999.2160p.BluRay.x265-GROUP","sizeWhenDone":1000,"percentDone":1.0,"downloadDir":"/downloads","files":[{"name":"matrix2.mkv"}]}
    ]}}"#.to_string());

    let mut target = torrent();
    target.size_bytes = Some(1000);

    let files = dl.completed_files(&target).unwrap();
    assert!(
        files.is_empty(),
        "未证实身份的同名同体积歧义任务绝不能被收集"
    );
}

#[test]
fn transmission_adds_to_an_explicit_download_directory() {
    let capture = Capture::new();
    let url = serve(capture.clone(), "/downloads".into());
    let dl = TransmissionDownloader::connect(TransmissionConfig {
        url,
        username: None,
        password: None,
        path_maps: vec![],
    })
    .unwrap();
    dl.add_with_options(
        &torrent(),
        "https://pt.example/download.php?id=1",
        Some("/downloads/manual"),
    )
    .unwrap();
    let requests = capture.requests.lock();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].contains("\"download-dir\":\"/downloads/manual\""));
}

#[test]
fn mixed_instances_coexist_and_optional_id_picks_default() {
    let dir = tempfile::tempdir().unwrap();
    let qbit_like = Arc::new(MemoryDownloader::new(dir.path().join("qbit")));
    let tr_like = Arc::new(MemoryDownloader::new(dir.path().join("tr")));
    let qbit_id = DownloaderId::new();
    let tr_id = DownloaderId::new();
    let set = DownloaderSet::new(
        qbit_id,
        [
            (qbit_id, qbit_like.clone() as Arc<dyn Downloader>),
            (tr_id, tr_like.clone() as Arc<dyn Downloader>),
        ]
        .into_iter()
        .collect(),
    )
    .unwrap();
    set.pick(None).unwrap().add(&torrent()).unwrap();
    set.pick(Some(tr_id)).unwrap().add(&torrent()).unwrap();
    assert_eq!(qbit_like.added().len(), 1);
    assert_eq!(tr_like.added().len(), 1);
}

#[test]
fn transmission_removes_matching_task() {
    let capture = Capture::new();
    let url = serve(capture.clone(), "/downloads".into());
    let dl = TransmissionDownloader::connect(TransmissionConfig {
        url,
        username: None,
        password: None,
        path_maps: vec![],
    })
    .unwrap();
    dl.add(&torrent()).unwrap();
    dl.remove(&torrent(), false).unwrap();
    let methods = capture.methods.lock().clone();
    assert!(
        methods.contains(&"torrent-remove".to_string()),
        "应调用 torrent-remove: {methods:?}"
    );
}

#[test]
fn transmission_removes_matching_task_with_delete_local_data() {
    let capture = Capture::new();
    let url = serve(capture.clone(), "/downloads".into());
    let dl = TransmissionDownloader::connect(TransmissionConfig {
        url,
        username: None,
        password: None,
        path_maps: vec![],
    })
    .unwrap();
    dl.add(&torrent()).unwrap();
    dl.remove(&torrent(), true).unwrap();
    let methods = capture.methods.lock().clone();
    assert!(
        methods.contains(&"torrent-remove".to_string()),
        "应调用 torrent-remove: {methods:?}"
    );
}

#[test]
fn transmission_remove_only_removes_uniquely_matched_torrent() {
    let capture = Capture::new();
    let url = serve(capture.clone(), "/downloads".into());
    let dl = TransmissionDownloader::connect(TransmissionConfig {
        url,
        username: None,
        password: None,
        path_maps: vec![],
    })
    .unwrap();

    // 客户端内有两个同体积(1000字节)任务：一个是要删的 Matrix，另一个是无关的 Other
    *capture.injected_torrents.lock() = Some(format!(
        r#"{{"result":"success","arguments":{{"torrents":[
        {{"id":1,"name":"The.Matrix.1999.2160p.BluRay.x265-GROUP","hashString":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","sizeWhenDone":1000,"labels":["crawler-media","{}"]}},
        {{"id":2,"name":"Other.Movie.2024.1080p","hashString":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","sizeWhenDone":1000,"labels":["crawler-media"]}}
    ]}}}}"#,
        downloader::ownership_tag("https://pt.example/download.php?id=1")
    ));

    let mut target = torrent();
    target.title = "The.Matrix.1999.2160p.BluRay.x265-GROUP".into();
    target.size_bytes = Some(1000);

    dl.remove(&target, true).unwrap();
    let removes: Vec<String> = capture
        .requests
        .lock()
        .iter()
        .filter(|r| r.contains("torrent-remove"))
        .cloned()
        .collect();
    assert_eq!(removes.len(), 1, "必须下发一次删除请求");
    let json_part = removes[0].split("\r\n\r\n").nth(1).unwrap();
    assert!(
        json_part.contains("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        "必须按已证实 hash 删除: {json_part}"
    );
    assert!(
        !json_part.contains("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
        "请求体绝不能包含无关 hash: {json_part}"
    );
}

#[test]
fn transmission_remove_ambiguous_matches_returns_error() {
    let capture = Capture::new();
    let url = serve(capture.clone(), "/downloads".into());
    let dl = TransmissionDownloader::connect(TransmissionConfig {
        url,
        username: None,
        password: None,
        path_maps: vec![],
    })
    .unwrap();

    // 客户端内有两个同名同体积的相同候选，无法消歧
    *capture.injected_torrents.lock() = Some(
        r#"{"result":"success","arguments":{"torrents":[
        {"id":1,"name":"The.Matrix.1999.2160p","sizeWhenDone":1000},
        {"id":2,"name":"The.Matrix.1999.2160p","sizeWhenDone":1000}
    ]}}"#
            .to_string(),
    );

    let mut target = torrent();
    target.title = "The.Matrix.1999.2160p".into();
    target.size_bytes = Some(1000);

    let err = dl.remove(&target, true).unwrap_err();
    assert!(
        err.to_string().contains("unproven"),
        "无精确身份时必须拒绝删除: {err}"
    );
    let removes: Vec<String> = capture
        .requests
        .lock()
        .iter()
        .filter(|r| r.contains("torrent-remove"))
        .cloned()
        .collect();
    assert!(removes.is_empty(), "歧义时绝不下发任何删除请求");
}

#[test]
fn transmission_handles_409_session_challenge() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    thread::spawn(move || {
        for incoming in listener.incoming() {
            let mut stream = incoming.unwrap();
            let req = read_request(&mut stream);
            let has_token = req.lines().any(|l| {
                let lower = l.to_ascii_lowercase();
                lower.starts_with("x-transmission-session-id:") && lower.contains("test-token-409")
            });
            let (status, body) = if has_token {
                (
                    "200 OK",
                    r#"{"result":"success","arguments":{"rpc-version":16}}"#,
                )
            } else {
                ("409 Conflict", "")
            };
            let response = format!(
                "HTTP/1.1 {status}\r\nX-Transmission-Session-Id: test-token-409\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).ok();
        }
    });

    let dl = TransmissionDownloader::connect(TransmissionConfig {
        url: format!("http://{addr}/transmission/rpc"),
        username: None,
        password: None,
        path_maps: vec![],
    })
    .unwrap();
    // 再次调用 rpc，必须复用 token，不触发第二次 409
    dl.add(&torrent()).unwrap();
}

#[test]
fn transmission_pause_and_resume_task_sends_torrent_stop_and_start() {
    let capture = Capture::new();
    let cap = capture.clone();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    thread::spawn(move || {
        for incoming in listener.incoming() {
            let mut stream = incoming.unwrap();
            let req = read_request(&mut stream);
            if let Some(body) = req.split("\r\n\r\n").nth(1) {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
                    if let Some(m) = v["method"].as_str() {
                        cap.methods.lock().push(m.to_string());
                    }
                }
            }
            let res = "HTTP/1.1 200 OK\r\nX-Transmission-Session-Id: test\r\nContent-Type: application/json\r\nContent-Length: 20\r\n\r\n{\"result\":\"success\"}";
            stream.write_all(res.as_bytes()).ok();
        }
    });

    let dl = TransmissionDownloader::connect(TransmissionConfig {
        url: format!("http://{addr}/transmission/rpc"),
        username: None,
        password: None,
        path_maps: vec![],
    })
    .unwrap();

    dl.pause_task("abcd1234abcd1234").unwrap();
    dl.resume_task("abcd1234abcd1234").unwrap();

    let methods = capture.methods.lock().clone();
    assert!(methods.contains(&"torrent-stop".to_string()));
    assert!(methods.contains(&"torrent-start".to_string()));
}

#[test]
fn transmission_completed_files_skips_deselected_or_incomplete_files() {
    let _capture = Capture::new();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    thread::spawn(move || {
        for incoming in listener.incoming() {
            let mut stream = incoming.unwrap();
            let req = read_request(&mut stream);
            let body = if req.contains("torrent-get") {
                // 模拟 1 个种子包含 2 个文件：
                // file 0: 完成，wanted=true
                // file 1: 未完成/被取消勾选，wanted=false, bytesCompleted=0
                r#"{
                    "result":"success",
                    "arguments":{
                        "torrents":[{
                            "id":1,
                            "name":"Show",
                            "hashString":"deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
                            "sizeWhenDone":1000,
                            "percentDone":1.0,
                            "downloadDir":"/downloads",
                            "labels":["crawler-media-task"],
                            "files":[
                                {"name":"Episode1.mkv","length":1000,"bytesCompleted":1000},
                                {"name":"Extra.mkv","length":500,"bytesCompleted":0}
                            ],
                            "fileStats":[
                                {"wanted":true,"bytesCompleted":1000},
                                {"wanted":false,"bytesCompleted":0}
                            ]
                        }]
                    }
                }"#
            } else {
                r#"{"result":"success","arguments":{"rpc-version":16}}"#
            };
            let res = format!(
                "HTTP/1.1 200 OK\r\nX-Transmission-Session-Id: test\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(res.as_bytes()).ok();
        }
    });

    let dl = TransmissionDownloader::connect(TransmissionConfig {
        url: format!("http://{addr}/transmission/rpc"),
        username: None,
        password: None,
        path_maps: vec![],
    })
    .unwrap();

    let mut t = torrent();
    t.enclosure = "magnet:?xt=urn:btih:deadbeefdeadbeefdeadbeefdeadbeefdeadbeef".into();

    let completed = dl.completed_files(&t).unwrap();
    assert_eq!(completed.len(), 1, "只应返回已完成且勾选的文件");
    assert_eq!(completed[0].to_str().unwrap(), "/downloads/Episode1.mkv");
}

/// D-H2: a duplicate task (added by hand) is claimed by merging our labels.
#[test]
fn transmission_duplicate_is_claimed_with_merged_labels() {
    let capture = Capture::new();
    *capture.injected_add.lock() = Some(
        r#"{"result":"success","arguments":{"torrent-duplicate":{"hashString":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","id":7}}}"#.into(),
    );
    *capture.injected_torrents.lock() = Some(
        r#"{"result":"success","arguments":{"torrents":[{"id":7,"hashString":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","labels":["mine"]}]}}"#.into(),
    );
    let dl = TransmissionDownloader::connect(TransmissionConfig {
        url: serve(capture.clone(), "/downloads".into()),
        username: None,
        password: None,
        path_maps: vec![],
    })
    .unwrap();
    dl.add(&torrent()).unwrap();
    let requests = capture.requests.lock();
    let set = requests.iter().find(|r| r.contains("torrent-set")).expect("torrent-set sent");
    let owned = downloader::ownership_tag(&torrent().enclosure);
    assert!(set.contains("mine") && set.contains(downloader::TASK_TAG) && set.contains(&owned), "{set}");
}
