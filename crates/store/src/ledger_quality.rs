use domain::{Confidence, LedgerRow, Release};
use rusqlite::{OptionalExtension, params};
use subscribe::SubscribeFacts;

use super::{Store, StoreError};

#[derive(serde::Serialize, serde::Deserialize)]
struct StoredQuality {
    resolution: Option<String>,
    source: Option<String>,
    codec: Option<String>,
    hdr: Option<String>,
}

impl From<&Release> for StoredQuality {
    fn from(release: &Release) -> Self {
        Self { resolution: release.resolution.clone(), source: release.source.clone(),
            codec: release.codec.clone(), hdr: release.hdr.clone() }
    }
}

impl Store {
    /// Persist provenance dimensions that stream probing cannot recover.
    /// The Library row owns this quality, so a later Subscribe can reuse it.
    pub(crate) fn persist_owned_quality(&self, facts: &SubscribeFacts) -> Result<(), StoreError> {
        let tx = self.library.unchecked_transaction()?;
        for (_, fact) in facts.entries() {
            let Some(path) = fact.path.as_deref() else { continue };
            let Some(quality) = facts.quality(path) else { continue };
            tx.execute(
                "UPDATE ledger SET release_quality = ?2 WHERE path = ?1",
                params![path, serde_json::to_string(&StoredQuality::from(quality))?],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Load stable Release facts and overlay the latest stream probe fields.
    pub(crate) fn owned_quality(&self, row: &LedgerRow) -> Result<Release, StoreError> {
        let json: Option<String> = self.library.query_row(
            "SELECT release_quality FROM ledger WHERE path = ?1", [&row.path], |r| r.get(0),
        ).optional()?.flatten();
        let mut quality = match json {
            Some(json) => {
                let stored: StoredQuality = serde_json::from_str(&json)?;
                Release {
                    title: String::new(), year: None, season: row.season, episode: row.episode,
                    episode_to: None, resolution: stored.resolution, source: stored.source,
                    codec: stored.codec, hdr: stored.hdr, subtitle_language: None,
                    audio_language: None, group: None, confidence: row.confidence,
                    season_inferred: false,
                }
            },
            None => Release {
                title: String::new(), year: None, season: row.season, episode: row.episode,
                episode_to: None, resolution: None, source: None, codec: None, hdr: None,
                subtitle_language: None, audio_language: None, group: None,
                confidence: Confidence::High,
                season_inferred: false,
            },
        };
        quality.season = row.season;
        quality.episode = row.episode;
        quality.resolution = row.resolution.clone().or(quality.resolution);
        quality.codec = row.codec.clone().or(quality.codec);
        quality.hdr = row.hdr.clone().or(quality.hdr);
        quality.confidence = row.confidence;
        Ok(quality)
    }
}
