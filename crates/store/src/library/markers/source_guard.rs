use super::{MarkerResultReplacement, Store, StoreError};
use rusqlite::{OptionalExtension, Transaction, params};
use std::collections::BTreeSet;

impl Store {
    pub fn complete_marker_refresh_checked(
        &self,
        job_id: &str,
        replacements: &[MarkerResultReplacement],
    ) -> Result<(), StoreError> {
        self.replace_marker_results_batch_inner(replacements, Some(job_id), true)
    }

    pub fn replace_marker_results_batch_checked(
        &self,
        replacements: &[MarkerResultReplacement],
    ) -> Result<(), StoreError> {
        self.replace_marker_results_batch_inner(replacements, None, true)
    }
}

pub(super) fn validate_members(
    tx: &Transaction<'_>,
    replacements: &[MarkerResultReplacement],
) -> Result<(), StoreError> {
    for replacement in replacements {
        let expected: BTreeSet<_> = replacement
            .chapter_updates
            .iter()
            .map(|(id, _)| id.clone())
            .collect();
        let mut statement = tx.prepare(
            "SELECT id, path FROM ledger WHERE media_id = ?1 AND COALESCE(season, 1) = ?2",
        )?;
        let current = statement
            .query_map(
                params![replacement.media_id.to_string(), replacement.season],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        let actual: BTreeSet<_> = current.iter().map(|(id, _)| id.clone()).collect();
        if actual != expected {
            return Err(StoreError::Missing(
                "file_deleted: season membership changed before publication".into(),
            ));
        }
        for (id, path) in current {
            match std::path::Path::new(&path).try_exists() {
                Ok(true) => {}
                Ok(false) => return Err(StoreError::Missing(format!("file_deleted: {id}"))),
                Err(error) => {
                    tracing::error!(%error, ledger_id = id, "检查标记发布源文件失败");
                    return Err(error.into());
                }
            }
        }
    }
    Ok(())
}

pub(super) fn validate_job_members(
    tx: &Transaction<'_>,
    job_id: &str,
    replacements: &[MarkerResultReplacement],
) -> Result<(), StoreError> {
    let published: BTreeSet<_> = replacements
        .iter()
        .flat_map(|replacement| replacement.chapter_updates.iter().map(|(id, _)| id.clone()))
        .collect();
    let mut statement = tx.prepare("SELECT ledger_id FROM probe_job_units WHERE job_id = ?1")?;
    let original: BTreeSet<String> = statement
        .query_map([job_id], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    if published != original {
        return Err(StoreError::Missing(
            "file_deleted: refresh membership changed before publication".into(),
        ));
    }
    Ok(())
}

pub(super) fn validate_published_rows(
    tx: &Transaction<'_>,
    replacements: &[MarkerResultReplacement],
) -> Result<(), StoreError> {
    let mut statement = tx.prepare("SELECT path FROM ledger WHERE id = ?1")?;
    for replacement in replacements {
        for (id, _) in &replacement.chapter_updates {
            let path_opt: Option<String> = statement
                .query_row(params![id], |row| row.get(0))
                .optional()?;
            let path = match path_opt {
                Some(path) => path,
                None => return Err(StoreError::Missing(format!("file_deleted: {id}"))),
            };
            match std::path::Path::new(&path).try_exists() {
                Ok(true) => {}
                Ok(false) => return Err(StoreError::Missing(format!("file_deleted: {id}"))),
                Err(error) => {
                    tracing::error!(%error, ledger_id = id, "检查标记发布源文件失败");
                    return Err(error.into());
                }
            }
        }
    }
    Ok(())
}
