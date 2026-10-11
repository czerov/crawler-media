//! Batch read facts for browse/playback consumers: media by id, and one
//! user's complete playback units.

use std::collections::HashMap;

use domain::{Media, MediaId, UserId};
use rusqlite::params;

use super::maps::{map_media, parse_id};
use super::playback::UnitState;
use super::schema::MEDIA_COLS;
use super::{Store, StoreError};

/// One playback unit with its media id and full per-unit state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaybackUnitSnapshot {
    pub media_id: MediaId,
    pub season: i32,
    pub episode: i32,
    pub state: UnitState,
}

const MEDIA_BATCH_CHUNK: usize = 500;

impl Store {
    /// Media rows for `ids`; unknown ids are simply absent, and empty input is
    /// an empty map. Ids are chunked to stay under SQLite's variable limit.
    pub fn media_batch(&self, ids: &[MediaId]) -> Result<HashMap<MediaId, Media>, StoreError> {
        let mut found = HashMap::new();
        for chunk in ids.chunks(MEDIA_BATCH_CHUNK) {
            let placeholders = vec!["?"; chunk.len()].join(", ");
            let sql = format!("SELECT {MEDIA_COLS} FROM media WHERE id IN ({placeholders})");
            let mut stmt = self.app.prepare(&sql)?;
            let rows = stmt.query_map(
                rusqlite::params_from_iter(chunk.iter().map(MediaId::to_string)),
                map_media,
            )?;
            for row in rows {
                let media = row?;
                found.insert(media.id, media);
            }
        }
        Ok(found)
    }

    /// Every playback unit for one user, with the same columns as
    /// `Store::unit_state` (position, play count, and track preferences
    /// included — not a reduced season/episode/played row).
    pub fn playback_units_for_user(
        &self,
        user: UserId,
    ) -> Result<Vec<PlaybackUnitSnapshot>, StoreError> {
        let mut stmt = self.subscribe.prepare(
            "SELECT media_id, season, episode,
                    position_ms, played, favorite, duration_ms, audio_track,
                    subtitle_track, play_count, updated_at
             FROM playback_units WHERE user_id = ?1
             ORDER BY media_id, season, episode",
        )?;
        let rows = stmt.query_map(params![user.to_string()], |row| {
            Ok(PlaybackUnitSnapshot {
                media_id: parse_id(row.get::<_, String>(0)?, 0)?,
                season: row.get(1)?,
                episode: row.get(2)?,
                state: UnitState {
                    position_ms: row.get(3)?,
                    played: row.get::<_, i64>(4)? != 0,
                    favorite: row.get::<_, i64>(5)? != 0,
                    duration_ms: row.get(6)?,
                    audio_track: row.get(7)?,
                    subtitle_track: row.get(8)?,
                    play_count: row.get(9)?,
                    updated_at: row.get(10)?,
                },
            })
        })?;
        let mut units = Vec::new();
        for row in rows {
            units.push(row?);
        }
        Ok(units)
    }
}
