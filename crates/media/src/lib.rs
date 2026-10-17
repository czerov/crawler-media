mod alias;
mod anilist;
mod bangumi;
mod cache;
mod client;
mod contracts;
mod douban;
pub mod fanart;
mod metadata;
mod parse;
mod person;
mod tvdb;

pub use alias::{Source, attach, merge};
pub use anilist::Anilist;
pub use bangumi::Bangumi;
pub use cache::DEFAULT_TTL_SECS;
pub use client::{CatalogGet, Tmdb, TmdbError};
pub use douban::Douban;
pub use parse::details;
pub use parse::episode_stills;
pub use parse::{
    CastRow, CatalogHit, CountryRow, EpisodeMeta, ImageCandidate, Images, ItemMeta, LanguageRow,
    SeasonEpisode, TvSeason,
};
pub use person::PersonDetails;
pub use tvdb::Tvdb;
