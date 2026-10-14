use std::str::FromStr;

use domain::{LedgerId, UserId};
use rusqlite::{OptionalExtension, params};

use super::{Store, StoreError};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaySessionIdState {
    Pending,
    Active,
    Closed,
}

impl PlaySessionIdState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Active => "active",
            Self::Closed => "closed",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "active" => Some(Self::Active),
            "closed" => Some(Self::Closed),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaySessionBinding {
    pub ledger_id: LedgerId,
    pub issued_at: i64,
    pub generation: i64,
    pub state: PlaySessionIdState,
}

impl Store {
    /// Issue a new play session ID for a (user, device, ledger) combination.
    ///
    /// Generates a UUID v4 ID, determines the next generation atomically in the
    /// subscribe.db immediate transaction, cleans expired IDs (up to 500), and records the pending ID.
    pub fn issue_play_session(
        &self,
        user_id: UserId,
        device_id: &str,
        ledger_id: LedgerId,
        now: i64,
    ) -> Result<String, StoreError> {
        let play_session_id = uuid::Uuid::new_v4().to_string();
        let user_str = user_id.to_string();
        let item_id_str = ledger_id.to_string();

        let tx = rusqlite::Transaction::new_unchecked(
            &self.subscribe,
            rusqlite::TransactionBehavior::Immediate,
        )?;

        // Clean expired sessions (older than 30 days = 30 * 86400 = 2_592_000 seconds)
        // only delete pending or closed, max 500
        let cutoff = now - 2_592_000;
        tx.execute(
            "DELETE FROM playback_session_ids
             WHERE rowid IN (
                 SELECT rowid FROM playback_session_ids
                 WHERE issued_at < ?1 AND state IN ('pending', 'closed')
                 ORDER BY issued_at ASC
                 LIMIT 500
             )",
            params![cutoff],
        )?;

        // Compute generation = MAX(generation) + 1 for (user_id, device_id)
        let max_gen: Option<i64> = tx
            .query_row(
                "SELECT MAX(generation) FROM playback_session_ids WHERE user_id = ?1 AND device_id = ?2",
                params![user_str, device_id],
                |r| r.get::<_, Option<i64>>(0),
            )
            .optional()?
            .flatten();
        let generation = max_gen.unwrap_or(0) + 1;

        tx.execute(
            "INSERT INTO playback_session_ids
                (user_id, device_id, play_session_id, item_id, issued_at, generation, state)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending')",
            params![user_str, device_id, play_session_id, item_id_str, now, generation],
        )?;

        tx.commit()?;
        Ok(play_session_id)
    }

    /// Retrieve binding info for a session ID given user and device.
    pub fn play_session_binding(
        &self,
        user_id: UserId,
        device_id: &str,
        play_session_id: &str,
    ) -> Result<Option<PlaySessionBinding>, StoreError> {
        self.subscribe
            .query_row(
                "SELECT item_id, issued_at, generation, state
                 FROM playback_session_ids
                 WHERE user_id = ?1 AND device_id = ?2 AND play_session_id = ?3",
                params![user_id.to_string(), device_id, play_session_id],
                |row| {
                    let item_str: String = row.get(0)?;
                    let issued_at: i64 = row.get(1)?;
                    let generation: i64 = row.get(2)?;
                    let state_str: String = row.get(3)?;
                    let ledger_id = LedgerId::from_str(&item_str)
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, 0))?;
                    let state = PlaySessionIdState::from_str(&state_str)
                        .unwrap_or(PlaySessionIdState::Closed);
                    Ok(PlaySessionBinding {
                        ledger_id,
                        issued_at,
                        generation,
                        state,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// Activate a play session ID.
    ///
    /// If the ID is already active, returns Ok(true) idempotently.
    /// If pending, verifies that no higher generation session has been activated on the device,
    /// marks older pending/active sessions on this device as closed, and marks this one active.
    /// If closed or unknown, returns Ok(false).
    pub fn activate_play_session(
        &self,
        user_id: UserId,
        device_id: &str,
        play_session_id: &str,
    ) -> Result<bool, StoreError> {
        if !self.subscribe.is_autocommit() {
            return self.activate_play_session_conn(
                &self.subscribe,
                user_id,
                device_id,
                play_session_id,
            );
        }
        let tx = rusqlite::Transaction::new_unchecked(
            &self.subscribe,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        let res = self.activate_play_session_conn(&tx, user_id, device_id, play_session_id)?;
        tx.commit()?;
        Ok(res)
    }

    fn activate_play_session_conn(
        &self,
        conn: &rusqlite::Connection,
        user_id: UserId,
        device_id: &str,
        play_session_id: &str,
    ) -> Result<bool, StoreError> {
        let current: Option<(i64, String)> = conn
            .query_row(
                "SELECT generation, state
                 FROM playback_session_ids
                 WHERE user_id = ?1 AND device_id = ?2 AND play_session_id = ?3",
                params![user_id.to_string(), device_id, play_session_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;

        let Some((generation, state_str)) = current else {
            return Ok(false);
        };

        if state_str == "active" {
            return Ok(true);
        }
        if state_str != "pending" {
            return Ok(false);
        }

        let max_active_gen: Option<i64> = conn
            .query_row(
                "SELECT MAX(generation)
                 FROM playback_session_ids
                 WHERE user_id = ?1 AND device_id = ?2 AND state = 'active'",
                params![user_id.to_string(), device_id],
                |r| r.get::<_, Option<i64>>(0),
            )
            .optional()?
            .flatten();

        if let Some(active_gen) = max_active_gen {
            if active_gen >= generation {
                conn.execute(
                    "UPDATE playback_session_ids SET state = 'closed'
                     WHERE user_id = ?1 AND device_id = ?2 AND play_session_id = ?3",
                    params![user_id.to_string(), device_id, play_session_id],
                )?;
                return Ok(false);
            }
        }

        conn.execute(
            "UPDATE playback_session_ids SET state = 'closed'
             WHERE user_id = ?1 AND device_id = ?2 AND generation < ?3 AND state IN ('pending', 'active')",
            params![user_id.to_string(), device_id, generation],
        )?;

        conn.execute(
            "UPDATE playback_session_ids SET state = 'active'
             WHERE user_id = ?1 AND device_id = ?2 AND play_session_id = ?3",
            params![user_id.to_string(), device_id, play_session_id],
        )?;

        Ok(true)
    }

    /// Mark a play session ID as closed.
    pub fn close_play_session_id(
        &self,
        user_id: UserId,
        device_id: &str,
        play_session_id: &str,
    ) -> Result<(), StoreError> {
        self.subscribe.execute(
            "UPDATE playback_session_ids SET state = 'closed'
             WHERE user_id = ?1 AND device_id = ?2 AND play_session_id = ?3",
            params![user_id.to_string(), device_id, play_session_id],
        )?;
        Ok(())
    }
}
