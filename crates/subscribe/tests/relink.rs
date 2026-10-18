use std::fs;
use std::path::Path;

use domain::{
    AtomRule, Coverage, FetchMode, Filter, FilterAtom, FilterId, Media, MediaId, MediaKind, SiteId,
    Subscribe, SubscribeId, Torrent, UserId,
};
use downloader::MemoryDownloader;
use filter::ScoredTorrent;
use library::{Ffprobe, TransferMode};
use subscribe::{Added, RunInput, RunOutcome, SubscribeFacts, collect_completed};

fn movie_filter() -> Filter {
    Filter {
        id: FilterId::new(),
        name: "movie".into(),
        atoms: vec![FilterAtom {
            priority: 100,
            rule: AtomRule::Resolution("1080p".into()),
            exclude: false,
        }],
        keep_old_versions: false,
}
}

fn media_movie() -> Media {
    Media {
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
    }
}

fn movie_sub(media: &Media, filter: &Filter) -> Subscribe {
    Subscribe {
        id: SubscribeId::new(),
        user_id: UserId::new(),
        media_id: media.id,
        coverage: Coverage::Movie,
        fetch_mode: FetchMode::Search,
        filter_id: filter.id,
        wash_cut: false,
        wash_cut_filter_id: None,
        full_season_pack: false,
        downloader_id: None,
        library_id: None,
        tracking_state: "active".into(),
        follow_future: false,
        search_interval_secs: 1800,
        keep_old_versions: false,
    }
}

fn torrent(enclosure: &str) -> Torrent {
    Torrent {
        id: None,
        site_id: SiteId::new(),
        title: "The.Matrix.1999.1080p.BluRay.x264.mkv".into(),
        enclosure: enclosure.into(),
        size_bytes: Some(1_000),
        seeders: Some(1),
        free: false,
        hr: false,
        imdb_id: None,
        leechers: None,
        snatched: None,
        upload_time: None,
        detail_url: None,
        category: None,
        poster_url: None,
    }
}

fn transfer_once(
    dl: &MemoryDownloader,
    filter: &Filter,
    media: &Media,
    sub: &Subscribe,
    t: &Torrent,
    facts: SubscribeFacts,
    library_root: &Path,
) -> RunOutcome {
    let input = RunInput {
        subscribe: sub,
        media,
        filter,
        wash_filter: None,
        torrents: vec![],
        search_keywords: vec![],
        facts,
        downloader: dl,
        library_root,
        transfer_mode: Some(TransferMode::Hardlink),
        scrape: false,
        hooks: None,
        naming: None,
        preserve_removed: false,
        overlap: Default::default(),
        absolute: None,
    };
    let added = Added {
        input,
        chosen: vec![ScoredTorrent {
            release: release::parse(&t.title),
            torrent: t.clone(),
            score: 100,
        }],
        add_errors: vec![],
        rejected: vec![],
        outcome: RunOutcome {
            facts: SubscribeFacts::default(),
            ledger: vec![],
            ledger_sources: vec![],
            removed_paths: vec![],
            completed: false,
            transferred_enclosures: vec![],
            rejected_enclosures: Vec::new(),
            awaiting_probe_enclosures: Vec::new(),
            collection_errors: vec![],
            submission_errors: vec![],
            torrents_added: vec![],
        },
        broken: Vec::new(),
    };
    collect_completed(added, &Ffprobe::default()).unwrap()
}

/// 删除即删除：库文件被删后，同一种子再次进入收集也**不会**被自动恢复。
/// 重新下载走「订阅立即搜索 / 手动投递」让 pending 回到 active，再转存一次。
#[test]
fn deleted_library_file_is_not_auto_restored() {
    let tmp = tempfile::tempdir().unwrap();
    let filter = movie_filter();
    let media = media_movie();
    let sub = movie_sub(&media, &filter);
    let t = torrent("magnet:?xt=urn:btih:relink-test");
    let src = tmp.path().join("The.Matrix.1999.1080p.mkv");
    fs::write(&src, b"video bytes").unwrap();
    let dl = MemoryDownloader::new(tmp.path().join("stage"));
    dl.map_enclosure_files(&t.enclosure, vec![src]);
    let library_root = tmp.path().join("lib");

    let outcome = transfer_once(
        &dl,
        &filter,
        &media,
        &sub,
        &t,
        SubscribeFacts::default(),
        &library_root,
    );
    assert_eq!(outcome.ledger.len(), 1);
    let dest = outcome.ledger[0].path.clone();
    assert!(Path::new(&dest).exists());

    // 用户删除库文件。
    fs::remove_file(&dest).unwrap();

    // 同样的种子再次进入收集：facts 仍指向该路径 → 跳过，不自动恢复。
    let second = transfer_once(&dl, &filter, &media, &sub, &t, outcome.facts, &library_root);
    assert!(!Path::new(&dest).exists(), "删除的库文件不得被自动恢复");
    assert!(second.ledger.is_empty(), "不得重新转存已删除的文件");
    assert!(second.transferred_enclosures.is_empty());
}
