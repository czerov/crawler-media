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
    // Look up media_id and library_ids associated with this ledger row
    let media_id_opt: Option<String> = tx
        .query_row(
            "SELECT media_id FROM ledger WHERE id = ?1",
            [id_str],
            |r| r.get(0),
        )
        .ok();

    // Mark affected browse generations dirty
    let _ = tx.execute(
        "UPDATE browse_generations SET state = 'dirty'
         WHERE library_id IN (SELECT library_id FROM browse_files WHERE ledger_id = ?1)",
        [id_str],
    );

    // Delete probe obligations in library.db
    tx.execute(
        "DELETE FROM ledger_probe_obligations WHERE ledger_id = ?1",
        [id_str],
    )?;
    // Delete browse_files in library.db
    tx.execute(
        "DELETE FROM browse_files WHERE ledger_id = ?1",
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

    // Invalidate/delete orphan browse_media if no ledger rows remain for this media
    if let Some(ref mid) = media_id_opt {
        tx.execute(
            "DELETE FROM browse_media
             WHERE media_id = ?1 AND NOT EXISTS (SELECT 1 FROM ledger WHERE media_id = ?1)",
            [mid],
        )?;
    }

    Ok(changed > 0)
}
