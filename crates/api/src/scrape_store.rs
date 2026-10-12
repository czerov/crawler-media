//! Store extensions for the scrape & organize configuration.
//! Separated from `crates/store` so that store does not depend on scrape composition rules.

use crate::scrape_config::{KEY, ScrapeConfig, ScrapeConfigSetting, compose_config};
use crate::store::{Store, StoreError};

pub trait ScrapeStoreExt {
    fn get_scrape_config(&self) -> Result<ScrapeConfig, StoreError>;
    fn save_scrape_config(&self, setting: &ScrapeConfigSetting)
    -> Result<ScrapeConfig, StoreError>;
    fn naming_pattern(&self, kind: domain::MediaKind) -> Result<String, StoreError>;
    fn set_legacy_naming(&self, kind: domain::MediaKind, pattern: &str) -> Result<(), StoreError>;
}

impl ScrapeStoreExt for Store {
    fn get_scrape_config(&self) -> Result<ScrapeConfig, StoreError> {
        let setting = match self.get_setting(KEY)? {
            None => ScrapeConfigSetting::default(),
            Some(raw) => match serde_json::from_str::<ScrapeConfigSetting>(&raw) {
                Ok(setting) => setting,
                Err(error) => {
                    // Fall back to defaults, but never rewrite the stored value:
                    // the corrupt raw text is the only copy an operator can
                    // repair, and silently persisting defaults would destroy it.
                    tracing::error!(
                        %error,
                        key = KEY,
                        "scrape config JSON is corrupt; falling back to defaults and keeping the raw value"
                    );
                    ScrapeConfigSetting::default()
                }
            },
        };
        Ok(compose_config(setting))
    }

    fn save_scrape_config(
        &self,
        setting: &ScrapeConfigSetting,
    ) -> Result<ScrapeConfig, StoreError> {
        self.put_setting(KEY, &serde_json::to_string(setting)?)?;
        Ok(compose_config(setting.clone()))
    }

    /// Composed full naming pattern for a media kind from the effective config.
    fn naming_pattern(&self, kind: domain::MediaKind) -> Result<String, StoreError> {
        let effective = self.get_scrape_config()?.effective;
        Ok(match kind {
            domain::MediaKind::Movie | domain::MediaKind::Video => {
                effective.compose_movie_pattern()
            }
            domain::MediaKind::Tv => effective.compose_tv_pattern(),
        })
    }

    /// Legacy `/directory` write entry: decompose an old-style composed pattern
    /// (may contain `/` hierarchy) back into the four scrape-config template
    /// fields. Empty fields stay "follow default".
    fn set_legacy_naming(&self, kind: domain::MediaKind, pattern: &str) -> Result<(), StoreError> {
        let mut setting = self.get_scrape_config()?.setting;
        apply_legacy_naming(&mut setting, kind, pattern);
        self.save_scrape_config(&setting)?;
        Ok(())
    }
}

/// Decompose an old-style composed pattern (may contain `/` hierarchy) back
/// into the four scrape-config template fields. Pure: no IO. Empty fields stay
/// "follow default"; the entry segment is shared, so a later TV call overrides
/// what a Movie call set (kept behavior, not a naming-rule change).
pub(crate) fn apply_legacy_naming(
    setting: &mut ScrapeConfigSetting,
    kind: domain::MediaKind,
    pattern: &str,
) {
    let segments: Vec<&str> = pattern.split('/').collect();
    match kind {
        domain::MediaKind::Movie | domain::MediaKind::Video => match segments.as_slice() {
            [entry, file] => {
                setting.naming_entry_dir = entry.trim().to_string();
                setting.naming_movie_file = file.trim().to_string();
            }
            [file] => setting.naming_movie_file = file.trim().to_string(),
            _ => setting.naming_movie_file = pattern.to_string(),
        },
        domain::MediaKind::Tv => match segments.as_slice() {
            [entry, season, file] => {
                setting.naming_entry_dir = entry.trim().to_string();
                setting.naming_season_dir = season.trim().to_string();
                setting.naming_episode_file = file.trim().to_string();
            }
            [entry, file] => {
                setting.naming_entry_dir = entry.trim().to_string();
                setting.naming_episode_file = file.trim().to_string();
            }
            [file] => setting.naming_episode_file = file.trim().to_string(),
            _ => setting.naming_episode_file = pattern.to_string(),
        },
    }
}
