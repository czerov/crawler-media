use domain::{Filter, FilterId, Site, SiteId};
use rusqlite::{OptionalExtension, params};

use super::maps::{map_site, parse_atoms, serialize_atoms};
use super::{Store, StoreError};

impl Store {
    pub fn insert_site(&self, site: &Site) -> Result<(), StoreError> {
        self.app.execute(
            "INSERT INTO sites (
                id, name, url, profile_id, cookie, api_key, rss_url, proxy,
                rate_limit_per_minute, cdp_url, downloader_id, enabled, maintenance_via
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                site.id.to_string(),
                site.name,
                site.url,
                site.profile_id,
                site.cookie,
                site.api_key,
                site.rss_url,
                site.proxy,
                site.rate_limit_per_minute,
                site.cdp_url,
                site.downloader_id.map(|id| id.to_string()),
                site.enabled as i64,
                site.maintenance_via.as_str(),
            ],
        )?;
        Ok(())
    }

    pub fn get_site(&self, id: SiteId) -> Result<Option<Site>, StoreError> {
        self.app
            .query_row(
                "SELECT id, name, url, profile_id, cookie, api_key, rss_url, proxy,
                        rate_limit_per_minute, cdp_url, downloader_id, enabled, maintenance_via
                 FROM sites WHERE id = ?1",
                params![id.to_string()],
                map_site,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn list_enabled_sites(&self) -> Result<Vec<Site>, StoreError> {
        let mut stmt = self.app.prepare(
            "SELECT id, name, url, profile_id, cookie, api_key, rss_url, proxy,
                    rate_limit_per_minute, cdp_url, downloader_id, enabled, maintenance_via
             FROM sites WHERE enabled = 1",
        )?;
        let rows = stmt.query_map([], map_site)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn list_sites(&self) -> Result<Vec<Site>, StoreError> {
        let mut stmt = self.app.prepare(
            "SELECT id, name, url, profile_id, cookie, api_key, rss_url, proxy,
                    rate_limit_per_minute, cdp_url, downloader_id, enabled, maintenance_via
             FROM sites",
        )?;
        let rows = stmt.query_map([], map_site)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn set_site_enabled(&self, id: SiteId, enabled: bool) -> Result<bool, StoreError> {
        let n = self.app.execute(
            "UPDATE sites SET enabled = ?2 WHERE id = ?1",
            params![id.to_string(), enabled as i64],
        )?;
        Ok(n > 0)
    }

    pub fn delete_site(&self, id: SiteId) -> Result<bool, StoreError> {
        let n = self
            .app
            .execute("DELETE FROM sites WHERE id = ?1", params![id.to_string()])?;
        Ok(n > 0)
    }

    pub fn save_site(&self, site: &Site) -> Result<bool, StoreError> {
        let n = self.app.execute(
            "UPDATE sites SET name = ?2, url = ?3, profile_id = ?4, cookie = ?5, api_key = ?6,
                    rss_url = ?7, proxy = ?8, rate_limit_per_minute = ?9, cdp_url = ?10,
                    downloader_id = ?11, enabled = ?12, maintenance_via = ?13
             WHERE id = ?1",
            params![
                site.id.to_string(),
                site.name,
                site.url,
                site.profile_id,
                site.cookie,
                site.api_key,
                site.rss_url,
                site.proxy,
                site.rate_limit_per_minute,
                site.cdp_url,
                site.downloader_id.map(|id| id.to_string()),
                site.enabled as i64,
                site.maintenance_via.as_str(),
            ],
        )?;
        Ok(n > 0)
    }

    pub fn insert_filter(&self, filter: &Filter) -> Result<(), StoreError> {
        let atoms = serialize_atoms(&filter.atoms);
        self.app.execute(
            "INSERT INTO filters (id, name, atoms_json, keep_old_versions) VALUES (?1, ?2, ?3, ?4)",
            params![
                filter.id.to_string(),
                filter.name,
                atoms,
                filter.keep_old_versions as i64
            ],
        )?;
        Ok(())
    }

    pub fn save_filter(&self, filter: &Filter) -> Result<(), StoreError> {
        let atoms = serialize_atoms(&filter.atoms);
        self.app.execute(
            "UPDATE filters SET name = ?1, atoms_json = ?2, keep_old_versions = ?3 WHERE id = ?4",
            params![
                filter.name,
                atoms,
                filter.keep_old_versions as i64,
                filter.id.to_string()
            ],
        )?;
        Ok(())
    }

    pub fn delete_filter(&self, id: FilterId) -> Result<bool, StoreError> {
        let n = self
            .app
            .execute("DELETE FROM filters WHERE id = ?1", params![id.to_string()])?;
        Ok(n > 0)
    }

    pub fn get_filter(&self, id: FilterId) -> Result<Option<Filter>, StoreError> {
        self.app
            .query_row(
                "SELECT id, name, atoms_json, keep_old_versions FROM filters WHERE id = ?1",
                params![id.to_string()],
                |row| {
                    Ok(Filter {
                        id: super::maps::parse_id(row.get(0)?, 0)?,
                        name: row.get(1)?,
                        atoms: parse_atoms(&row.get::<_, String>(2)?),
                        keep_old_versions: row.get::<_, i64>(3).unwrap_or(0) != 0,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn list_filters(&self) -> Result<Vec<Filter>, StoreError> {
        let mut stmt = self
            .app
            .prepare("SELECT id, name, atoms_json, keep_old_versions FROM filters")?;
        let rows = stmt.query_map([], |row| {
            Ok(Filter {
                id: super::maps::parse_id(row.get(0)?, 0)?,
                name: row.get(1)?,
                atoms: parse_atoms(&row.get::<_, String>(2)?),
                keep_old_versions: row.get::<_, i64>(3).unwrap_or(0) != 0,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }
}
