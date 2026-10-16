mod browser;
pub mod browser_host;
pub mod cdp;
pub mod cdp_page;
mod engine;
mod fetch;
mod parse;
mod profile;

pub use browser::{Browser, BrowserConfig, BrowserRoute, PageSession, RecordingFetcher, RenderedPage};
pub use cdp::{CdpCookie, DiscoveredSiteCookie, fetch_cookies_from_cdp, match_cookies_to_profiles};
pub use engine::{Indexer, SearchOutcome, SiteFailure};
pub use fetch::{FetchMethod, FetchRequest, Fetcher};
pub use profile::{Framework, Profile, ProfileSet};

#[derive(Debug, thiserror::Error)]
pub enum IndexerError {
    #[error("Browser busy")]
    BrowserBusy,
    #[error("Browser setup: {0}")]
    BrowserSetup(String),
    #[error("{0}")]
    Fetch(String),
    #[error("parse: {0}")]
    Parse(String),
    #[error("unknown profile: {0}")]
    UnknownProfile(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Yaml(#[from] serde_yaml::Error),
}
