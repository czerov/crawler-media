use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;

use domain::{
    AtomRule, Coverage, FetchMode, Filter, FilterAtom, FilterId, Media, MediaId, MediaKind, SiteId,
    Subscribe, SubscribeId, Torrent, UserId,
};
use downloader::{QbitConfig, QbitDownloader};
use library::TransferMode;
use subscribe::{RunInput, SubscribeFacts, run};

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
        if expected.is_none()
            && let Some(header_end) = request.windows(4).position(|w| w == b"\r\n\r\n")
        {
            let headers = String::from_utf8_lossy(&request[..header_end]);
            let content_length = headers.lines().find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            });
            expected = Some(header_end + 4 + content_length.unwrap_or(0));
        }
        if expected.is_some_and(|length| request.len() >= length) {
            break;
        }
    }
    String::from_utf8_lossy(&request).into_owned()
}

fn serve_qbit(save_path: String) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let request = read_request(&mut stream);
            let (body, cookie) = if request.starts_with("POST /api/v2/auth/login") {
                ("Ok.".to_string(), "Set-Cookie: SID=test; HttpOnly\r\n")
            } else if request.starts_with("POST /api/v2/torrents/add") {
                ("Ok.".to_string(), "")
            } else if request.contains("/api/v2/torrents/info") {
                let tag = downloader::ownership_tag("https://pt.example/download.php?id=1");
                (
                    format!(
                        r#"[{{"hash":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","name":"The.Matrix.1999.2160p.BluRay.x265-GROUP","size":1000,"progress":1.0,"save_path":"{save_path}","tags":"crawler-media,{tag}"}}]"#
                    ),
                    "",
                )
            } else if request.contains("/api/v2/torrents/files") {
                (
                    r#"[{"name":"finished.mkv","progress":1.0}]"#.to_string(),
                    "",
                )
            } else {
                ("[]".to_string(), "")
            };
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n{cookie}\r\n{body}",
                body.len(),
            );
            stream.write_all(response.as_bytes()).unwrap();
        }
    });
    format!("http://{addr}")
}

#[test]
fn finished_qbittorrent_task_transfers_into_library_ledger() {
    let tmp = tempfile::tempdir().unwrap();
    let finished = tmp.path().join("downloads/finished.mkv");
    std::fs::create_dir_all(finished.parent().unwrap()).unwrap();
    std::fs::write(
        &finished,
        r#"{"resolution":"2160p","codec":"hevc","hdr":"hdr10"}"#,
    )
    .unwrap();
    let qbit = QbitDownloader::connect(QbitConfig {
        url: serve_qbit(
            finished
                .parent()
                .unwrap()
                .display()
                .to_string()
                .replace('\\', "/"),
        ),
        username: "admin".into(),
        password: "pass".into(),
        category: Some("crawler-media".into()),
        path_maps: vec![],
    })
    .unwrap();
    let media = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "The Matrix".into(),
        year: None,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    let filter = Filter {
        id: FilterId::new(),
        name: "uhd".into(),
        atoms: vec![FilterAtom {
            priority: 100,
            rule: AtomRule::Resolution("2160p".into()),
            exclude: false,
        }],
        keep_old_versions: false,
};
    let subscribe = Subscribe {
        id: SubscribeId::new(),
        user_id: UserId::new(),
        media_id: media.id,
        coverage: Coverage::Movie,
        fetch_mode: FetchMode::Search,
        filter_id: filter.id,
        wash_cut: false,
        keep_old_versions: false,
        wash_cut_filter_id: None,
        full_season_pack: false,
        downloader_id: None,
        library_id: None,
        tracking_state: "active".into(),
        follow_future: false,
        search_interval_secs: 1800,
    };
    let torrent = Torrent {
        site_id: SiteId::new(),
        title: "The.Matrix.1999.2160p.BluRay.x265-GROUP".into(),
        enclosure: "https://pt.example/download.php?id=1".into(),
        size_bytes: Some(1000),
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
    };
    let library_root = tmp.path().join("library");

    let outcome = run(RunInput {
        subscribe: &subscribe,
        media: &media,
        filter: &filter,
        wash_filter: None,
        torrents: vec![torrent],
        search_keywords: vec![],
        facts: SubscribeFacts::default(),
        downloader: &qbit,
        library_root: &library_root,
        transfer_mode: Some(TransferMode::Copy),
        scrape: false,
        hooks: None,
        naming: None,
        preserve_removed: false,
        overlap: Default::default(),
        absolute: None,
    })
    .unwrap();

    assert_eq!(outcome.ledger.len(), 1);
    assert!(std::path::Path::new(&outcome.ledger[0].path).is_file());
    assert_eq!(outcome.ledger[0].media_id, media.id);
    assert_eq!(outcome.ledger[0].resolution.as_deref(), Some("2160p"));
}
