mod support;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use api::HttpFetcher;
use downloader::{QbitConfig, QbitDownloader};
use indexer::{Indexer, ProfileSet};
use library::TransferMode;
use subscribe::{RunInput, SubscribeFacts, run};
use support::{choose, env, print_hits, site, subscribe_bundle};

fn main() {
    if let Err(error) = run_live() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run_live() -> Result<(), Box<dyn std::error::Error>> {
    let keyword = std::env::var("LIVE_KEYWORD").unwrap_or_else(|_| "The Long Watch S01E01".into());
    let library_root = PathBuf::from(env("LIVE_LIBRARY"));
    std::fs::create_dir_all(&library_root)?;

    let overlay = std::env::var("LIVE_PROFILES")
        .ok()
        .map(PathBuf::from)
        .filter(|path| path.is_dir());
    let mteam = site(
        "M-Team",
        env("MTEAM_URL"),
        "mteam",
        None,
        Some(env("MTEAM_API_KEY")),
    );
    let pter = site(
        "PTerClub",
        env("PTER_URL"),
        "pterclub",
        Some(env("PTER_COOKIE")),
        None,
    );
    let indexer = Indexer::new(ProfileSet::load(overlay.as_deref())?, Arc::new(HttpFetcher));

    println!("searching M-Team for {keyword:?}");
    let mt = indexer.search(&[mteam], &keyword);
    print_hits("M-Team", &mt);
    println!("searching PTerClub for {keyword:?}");
    let pt = indexer.search(&[pter], &keyword);
    print_hits("PTerClub", &pt);

    let mut torrents = mt.torrents;
    torrents.extend(pt.torrents);
    let chosen = choose(torrents)?;
    println!("chosen {} ({:?} bytes)", chosen.title, chosen.size_bytes);

    let qb = QbitDownloader::connect(QbitConfig {
        url: env("QB_URL"),
        username: env("QB_USER"),
        password: env("QB_PASS"),
        category: Some("crawler-live".into()),
        path_maps: downloader::parse_path_maps(&env("QB_PATH_MAP")),
    })?;
    let (media, filter, subscribe) = subscribe_bundle();
    let deadline = Instant::now() + Duration::from_secs(15 * 60);
    loop {
        let outcome = run(RunInput {
            subscribe: &subscribe,
            media: &media,
            filter: &filter,
            wash_filter: None,
            torrents: vec![chosen.clone()],
            search_keywords: vec![],
            facts: SubscribeFacts::default(),
            downloader: &qb,
            library_root: &library_root,
            transfer_mode: Some(TransferMode::Copy),
            scrape: true,
            hooks: None,
            naming: None,
            preserve_removed: false,
            overlap: Default::default(),
            absolute: None,
        })?;
        println!(
            "run ledger={} completed={}",
            outcome.ledger.len(),
            outcome.completed
        );
        if !outcome.ledger.is_empty() {
            for row in &outcome.ledger {
                println!(
                    "ledger {} S{:?}E{:?} {}",
                    row.path,
                    row.season,
                    row.episode,
                    row.confidence.as_str()
                );
            }
            return Ok(());
        }
        if Instant::now() > deadline {
            return Err("timed out waiting for download".into());
        }
        std::thread::sleep(Duration::from_secs(5));
    }
}
