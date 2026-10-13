//! Transactional cleanup of file-derived rows in library.db for a ledger item.
//!
//! Deletes file_meta, fingerprint caches/attempts/samples/stages, scrape_sidecars
//! associations, and the ledger entry itself in a single transaction.
//! Does NOT delete Media, Playback, Collections, Markers or disk bytes.

use domain::LedgerId;
use rusqlite::Transaction;

use crate::StoreError;

/// Delete all file-derived facts and the ledger row itself for `id` within `tx`.
///
/// Returns `Ok(true)` if a ledger row was deleted, `Ok(false)` if no such row existed.
pub(crate) fn delete_ledger_in_tx(
    tx: &Transaction<'_>,
    id: LedgerId,
) -> Result<bool, StoreError> {
    let id_str = id.to_string();
    delete_ledger_by_id_str_in_tx(tx, &id_str)
}

/// Helper taking string id representation.
pub(crate) fn delete_ledger_by_id_str_in_tx(
    tx: &Transaction<'_>,
    id_str: &str,
) -> Result<bool, StoreError> {
    // Delete probe obligations in library.db
    tx.execute(
        "DELETE FROM ledger_probe_obligations WHERE ledger_id = ?1",
        [id_str],
    )?;
    // Delete file-derived caches and metadata
    crate::library::delete_file_meta_for_ledger(tx, id_str)?;

    // Delete scrape_sidecars association in library.db
    tx.execute(
        "DELETE FROM scrape_sidecars WHERE ledger_id = ?1",
        [id_str],
    )?;

    // Delete the ledger row itself
    let changed = tx.execute("DELETE FROM ledger WHERE id = ?1", [id_str])?;
    Ok(changed > 0)
}
