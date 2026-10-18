//! Catalog (TMDB stand-in) with per-episode air dates, plus TV helpers.

use std::sync::Arc;

use api::catalog::Catalog;
use domain::SubscribeId;
use parking_lot::Mutex;
use serde_json::{Value, json};

use super::harness::{Harness, TOKEN};

#[derive(Default)]
pub struct FakeCatalog {
    /// season -> [(episode, air_date YYYY-MM-DD)]
    pub episodes: Mutex<Vec<(u32, u32, Option<String>)>>,
}

impl FakeCatalog {
    pub fn with_episodes(rows: &[(u32, u32, Option<String>)]) -> Arc<Self> {
        Arc::new(Self {
            episodes: Mutex::new(rows.to_vec()),
        })
    }
}

impl Catalog for FakeCatalog {
    fn search_movie(&self, _: &str) -> Result<Vec<media::CatalogHit>, String> {
        Ok(vec![])
    }
    fn search_tv(&self, _: &str) -> Result<Vec<media::CatalogHit>, String> {
        Ok(vec![])
    }
    fn popular_movie(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(vec![])
    }
    fn popular_tv(&self) -> Result<Vec<media::CatalogHit>, String> {
        Ok(vec![])
    }
    fn details(&self, _: domain::MediaKind, _: &str) -> Result<Option<domain::Media>, String> {
        Ok(None)
    }
    fn season_episodes(&self, _: &str, season: u32) -> Result<Vec<media::SeasonEpisode>, String> {
        Ok(self
            .episodes
            .lock()
            .iter()
            .filter(|(s, _, _)| *s == season)
            .map(|(_, ep, air)| media::SeasonEpisode {
                episode_number: *ep,
                air_date: air.clone(),
            })
            .collect())
    }
}

/// `YYYY-MM-DD` for today (UTC) shifted by `days`.
pub fn date_in(days: i64) -> String {
    let secs = api::job_loop::unix_now() + days * 86_400;
    let z = secs.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// TV Subscribe for "The Long Watch" (1080p filter) with `coverage` and extra fields.
pub async fn tv_subscribe(h: &Harness, coverage: Value, extra: Value) -> SubscribeId {
    let mut payload = json!({
        "media": { "kind": "tv", "title": "The Long Watch", "tmdb_id": "100" },
        "coverage": coverage,
        "fetch_mode": "search",
        "filter": {
            "name": "long-watch-1080p",
            "atoms": [
                { "kind": "resolution", "value": "1080p", "priority": 100 },
                { "kind": "title_match", "value": "Long", "priority": 80 }
            ]
        },
        "wash_cut": false,
        "full_season_pack": false
    });
    if let (Some(target), Some(extra)) = (payload.as_object_mut(), extra.as_object()) {
        for (k, v) in extra {
            target.insert(k.clone(), v.clone());
        }
    }
    let response = tower::ServiceExt::oneshot(
        h.app.clone(),
        super::common::request("POST", "/api/v1/subscriptions", Some(TOKEN), payload),
    )
    .await
    .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::CREATED);
    super::common::json_data(response).await["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap()
}
