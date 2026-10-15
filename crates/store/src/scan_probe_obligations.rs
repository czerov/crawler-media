//! Scanned ledger insertion with atomic probe obligations and dispatching.

use std::str::FromStr;

use domain::{LedgerId, LedgerRow};
use rusqlite::params;

use super::{Store, StoreError};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanProbeObligation {
    pub ledger_id: LedgerId,
    pub created_at_ms: i64,
    pub reason: String,
}

impl Store {
    /// Atomically insert a scanned ledger row and record its probe obligation
    /// in a single Immediate transaction in library.db.
    pub fn insert_scanned_ledger(
        &self,
        row: &LedgerRow,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        let tx = rusqlite::Transaction::new_unchecked(
            &self.library,
            rusqlite::TransactionBehavior::Immediate,
        )?;

        // Insert ledger row (if path exists, ignore / fail per contract)
        tx.execute(
            "INSERT INTO ledger (
                 id, media_id, path, season, episode, resolution, codec, hdr,
                 quality_source, confidence, filter_score
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                row.id.to_string(),
                row.media_id.to_string(),
                row.path,
                row.season,
                row.episode,
                row.resolution,
                row.codec,
                row.hdr,
                row.quality_source.as_str(),
                row.confidence.as_str(),
                row.filter_score,
            ],
        )?;

        // Insert probe obligation
        tx.execute(
            "INSERT INTO ledger_probe_obligations (ledger_id, created_at_ms, reason)
             VALUES (?1, ?2, 'scan')",
            params![row.id.to_string(), now_ms],
        )?;

        tx.commit()?;
        Ok(())
    }

    /// Retrieve up to `limit` (clamped 1..=100) pending probe obligations in deterministic order.
    pub fn scan_probe_obligations(&self, limit: usize) -> Result<Vec<ScanProbeObligation>, StoreError> {
        self.scan_probe_obligations_with_vm_steps(limit)
            .map(|(obligations, _)| obligations)
    }

    /// Same read as [`Self::scan_probe_obligations`], additionally reporting the
    /// SQLite VM steps the query consumed so tests can assert DB work.
    #[doc(hidden)]
    pub fn scan_probe_obligations_with_vm_steps(
        &self,
        limit: usize,
    ) -> Result<(Vec<ScanProbeObligation>, i64), StoreError> {
        let limit = limit.clamp(1, 100);
        let mut stmt = self.library.prepare(
            "SELECT ledger_id, created_at_ms, reason
             FROM ledger_probe_obligations
             ORDER BY created_at_ms ASC, ledger_id ASC
             LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], |row| {
            let id_str: String = row.get(0)?;
            let created_at_ms: i64 = row.get(1)?;
            let reason: String = row.get(2)?;
            let ledger_id = LedgerId::from_str(&id_str)
                .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, 0))?;
            Ok(ScanProbeObligation {
                ledger_id,
                created_at_ms,
                reason,
            })
        })?;
        let obligations = rows.collect::<Result<Vec<_>, _>>()?;
        let vm_steps = i64::from(stmt.get_status(rusqlite::StatementStatus::VmStep));
        Ok((obligations, vm_steps))
    }

    /// Acknowledge / remove a fulfilled probe obligation.
    pub fn ack_scan_probe_obligation(&self, ledger_id: LedgerId) -> Result<(), StoreError> {
        self.library.execute(
            "DELETE FROM ledger_probe_obligations WHERE ledger_id = ?1",
            params![ledger_id.to_string()],
        )?;
        Ok(())
    }
}
