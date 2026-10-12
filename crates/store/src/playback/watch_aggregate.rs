//! SQL aggregation for the playback watch-statistics report.
//!
//! Replaces the two full `list_logs(200_000, ..)` scans plus in-Rust grouping
//! with one windowed aggregation on the subscribe connection. Visibility reuses
//! the TEMP visible-set pattern from `playback::log_scope`.

use std::collections::HashMap;

use domain::{MediaId, UserId};
use rusqlite::named_params;

use super::{Store, StoreError, media_id_from, user_id_from};

/// 单次统计允许的最大窗口天数与最大时区偏移（分钟）。
const MAX_DAYS: u16 = 365;
const MAX_TZ_MINUTES: i16 = 24 * 60;
/// TEMP 可见集合一次 INSERT 的批量大小。
const VISIBLE_CHUNK: usize = 500;

const VISIBLE_CLAUSE: &str =
    " AND EXISTS (SELECT 1 FROM request_visible_media v WHERE v.id = l.media_id)";
const HIDDEN_CLAUSE: &str =
    " AND NOT EXISTS (SELECT 1 FROM request_visible_media v WHERE v.id = l.media_id)";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WatchTotals {
    pub plays: i64,
    pub watched_ms: i64,
    pub completed: i64,
    pub members: i64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WatchDay {
    pub day: i64,
    pub totals: WatchTotals,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WatchMember {
    pub user_id: UserId,
    pub totals: WatchTotals,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WatchClient {
    pub client: String,
    pub plays: i64,
    pub watched_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WatchMedia {
    pub media_id: MediaId,
    pub plays: i64,
    pub watched_ms: i64,
    pub members: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WatchWindow {
    pub totals: WatchTotals,
    pub days: Vec<WatchDay>,
    /// 7×24 的 watched_ms 矩阵：`hours[weekday][hour]`。
    pub hours: Vec<Vec<i64>>,
    pub users: Vec<WatchMember>,
    pub clients: Vec<WatchClient>,
    pub media: Vec<WatchMedia>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WatchReport {
    pub current: WatchWindow,
    pub previous: WatchWindow,
    /// 当前窗口内不可见 Media 的 distinct 数量。
    pub hidden_titles: usize,
}

/// `scoped` CTE：把时区偏移一次算好，day/hour 与 Rust 的
/// `div_euclid` / `rem_euclid` 语义一致。
///
/// SQL 的 `/` 对负数是向零截断，所以 `div_euclid(86400)` 必须写成 CASE：
/// 非负直接除，负值用 `-(((-x)+86399)/86400)` 取 floor。
/// `rem_euclid` 用 `((x % n) + n) % n`。
fn scoped_cte(visibility: &str) -> String {
    format!(
        "WITH scoped AS (
            SELECT l.user_id, l.client, l.media_id, l.watched_ms, l.completed,
                   CASE WHEN l.started_at + :tz >= 0
                        THEN (l.started_at + :tz) / 86400
                        ELSE -(((-(l.started_at + :tz)) + 86399) / 86400) END AS day,
                   ((l.started_at + :tz) % 86400 + 86400) % 86400 / 3600 AS hour
            FROM playback_logs l
            WHERE l.started_at >= :start
              AND l.started_at < :end
              AND (:user IS NULL OR l.user_id = :user){visibility}
        )"
    )
}

impl Store {
    /// 当前窗口 + 上一窗口的播放统计（SQL 聚合，不把日志读进内存）。
    ///
    /// - `user`: `None` = 全部成员，`Some` = 只统计该成员。
    /// - `visible`: `None` = 不限制；`Some` = 只统计集合内的 Media（空集合 → 全隐藏）。
    /// - 当前窗口 `started_at >= now - days*86400`（无上界，保持既有语义）；
    ///   上一窗口 `[now - 2*days*86400, now - days*86400)`。
    pub fn playback_watch_report(
        &self,
        user: Option<UserId>,
        visible: Option<&[MediaId]>,
        now: i64,
        days: u16,
        tz_minutes: i16,
    ) -> Result<WatchReport, StoreError> {
        if days == 0 || days > MAX_DAYS {
            return Err(StoreError::Invalid(format!(
                "watch window days must be 1..={MAX_DAYS}, got {days}"
            )));
        }
        if tz_minutes.abs() > MAX_TZ_MINUTES {
            return Err(StoreError::Invalid(format!(
                "tz offset minutes must be within ±{MAX_TZ_MINUTES}, got {tz_minutes}"
            )));
        }
        let window = i64::from(days) * 86_400;
        let tz_secs = i64::from(tz_minutes) * 60;
        let today = (now + tz_secs).div_euclid(86_400);
        let user_text = user.map(|id| id.to_string());

        // 与 log_scope 同一套做法：SAVEPOINT 之外先清空 TEMP 可见集合，
        // 错误路径回滚后再清一次，保证 scope 行不会残留给下一次调用。
        self.reset_watch_visible_media()?;
        self.subscribe.execute_batch("SAVEPOINT watch_aggregate")?;
        let result = self.watch_report_inner(
            &user_text, visible, now, days, tz_secs, today, window,
        );
        match result {
            Ok(report) => {
                self.subscribe.execute_batch("RELEASE watch_aggregate")?;
                Ok(report)
            }
            Err(error) => {
                let _ = self
                    .subscribe
                    .execute_batch("ROLLBACK TO watch_aggregate; RELEASE watch_aggregate");
                let _ = self.reset_watch_visible_media();
                Err(error)
            }
        }
    }

    fn watch_report_inner(
        &self,
        user: &Option<String>,
        visible: Option<&[MediaId]>,
        now: i64,
        days: u16,
        tz_secs: i64,
        today: i64,
        window: i64,
    ) -> Result<WatchReport, StoreError> {
        if let Some(ids) = visible {
            self.fill_watch_visible_media(ids)?;
        }
        let visibility = if visible.is_some() { VISIBLE_CLAUSE } else { "" };
        let current = self.window_report(
            user,
            now - window,
            i64::MAX,
            days,
            today - i64::from(days) + 1,
            tz_secs,
            visibility,
        )?;
        let previous = self.window_report(
            user,
            now - 2 * window,
            now - window,
            days,
            today - 2 * i64::from(days) + 1,
            tz_secs,
            visibility,
        )?;
        let hidden_titles = if visible.is_some() {
            let sql = format!("{} SELECT COUNT(DISTINCT media_id) FROM scoped", scoped_cte(HIDDEN_CLAUSE));
            let mut stmt = self.subscribe.prepare(&sql)?;
            let count: i64 = stmt.query_row(
                named_params! { ":tz": tz_secs, ":start": now - window, ":end": i64::MAX, ":user": user },
                |row| row.get(0),
            )?;
            count.max(0) as usize
        } else {
            0
        };
        Ok(WatchReport {
            current,
            previous,
            hidden_titles,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn window_report(
        &self,
        user: &Option<String>,
        start: i64,
        end: i64,
        days: u16,
        first_day: i64,
        tz_secs: i64,
        visibility: &str,
    ) -> Result<WatchWindow, StoreError> {
        let cte = scoped_cte(visibility);
        Ok(WatchWindow {
            totals: self.window_totals(&cte, user, start, end, tz_secs)?,
            days: self.window_days(&cte, user, start, end, tz_secs, days, first_day)?,
            hours: self.window_hours(&cte, user, start, end, tz_secs)?,
            users: self.window_users(&cte, user, start, end, tz_secs)?,
            clients: self.window_clients(&cte, user, start, end, tz_secs)?,
            media: self.window_media(&cte, user, start, end, tz_secs)?,
        })
    }

    fn window_totals(
        &self,
        cte: &str,
        user: &Option<String>,
        start: i64,
        end: i64,
        tz_secs: i64,
    ) -> Result<WatchTotals, StoreError> {
        let sql = format!(
            "{cte} SELECT COUNT(*), COALESCE(SUM(watched_ms), 0),
                    COALESCE(SUM(completed), 0), COUNT(DISTINCT user_id) FROM scoped"
        );
        let mut stmt = self.subscribe.prepare(&sql)?;
        Ok(stmt.query_row(
            named_params! { ":tz": tz_secs, ":start": start, ":end": end, ":user": user },
            |row| {
                Ok(WatchTotals {
                    plays: row.get(0)?,
                    watched_ms: row.get(1)?,
                    completed: row.get(2)?,
                    members: row.get(3)?,
                })
            },
        )?)
    }

    #[allow(clippy::too_many_arguments)]
    fn window_days(
        &self,
        cte: &str,
        user: &Option<String>,
        start: i64,
        end: i64,
        tz_secs: i64,
        days: u16,
        first_day: i64,
    ) -> Result<Vec<WatchDay>, StoreError> {
        let sql = format!(
            "{cte} SELECT day, COUNT(*), COALESCE(SUM(watched_ms), 0),
                    COALESCE(SUM(completed), 0), COUNT(DISTINCT user_id)
             FROM scoped GROUP BY day"
        );
        let mut stmt = self.subscribe.prepare(&sql)?;
        let rows = stmt.query_map(
            named_params! { ":tz": tz_secs, ":start": start, ":end": end, ":user": user },
            |row| {
                Ok(WatchDay {
                    day: row.get(0)?,
                    totals: WatchTotals {
                        plays: row.get(1)?,
                        watched_ms: row.get(2)?,
                        completed: row.get(3)?,
                        members: row.get(4)?,
                    },
                })
            },
        )?;
        let mut found: HashMap<i64, WatchTotals> = HashMap::new();
        for row in rows {
            let entry = row?;
            found.insert(entry.day, entry.totals);
        }
        Ok((0..i64::from(days))
            .map(|offset| {
                let day = first_day + offset;
                WatchDay {
                    day,
                    totals: found.get(&day).copied().unwrap_or_default(),
                }
            })
            .collect())
    }

    fn window_hours(
        &self,
        cte: &str,
        user: &Option<String>,
        start: i64,
        end: i64,
        tz_secs: i64,
    ) -> Result<Vec<Vec<i64>>, StoreError> {
        let sql = format!(
            "{cte} SELECT ((day + 3) % 7 + 7) % 7 AS weekday, hour,
                    COALESCE(SUM(watched_ms), 0)
             FROM scoped GROUP BY weekday, hour"
        );
        let mut stmt = self.subscribe.prepare(&sql)?;
        let rows = stmt.query_map(
            named_params! { ":tz": tz_secs, ":start": start, ":end": end, ":user": user },
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?)),
        )?;
        let mut hours = vec![vec![0i64; 24]; 7];
        for row in rows {
            let (weekday, hour, watched) = row?;
            if (0..7).contains(&weekday) && (0..24).contains(&hour) {
                hours[weekday as usize][hour as usize] = watched;
            }
        }
        Ok(hours)
    }

    fn window_users(
        &self,
        cte: &str,
        user: &Option<String>,
        start: i64,
        end: i64,
        tz_secs: i64,
    ) -> Result<Vec<WatchMember>, StoreError> {
        let sql = format!(
            "{cte} SELECT user_id, COUNT(*), COALESCE(SUM(watched_ms), 0),
                    COALESCE(SUM(completed), 0)
             FROM scoped GROUP BY user_id ORDER BY user_id"
        );
        let mut stmt = self.subscribe.prepare(&sql)?;
        let rows = stmt.query_map(
            named_params! { ":tz": tz_secs, ":start": start, ":end": end, ":user": user },
            |row| {
                Ok(WatchMember {
                    user_id: user_id_from(row, 0)?,
                    totals: WatchTotals {
                        plays: row.get(1)?,
                        watched_ms: row.get(2)?,
                        completed: row.get(3)?,
                        members: 1,
                    },
                })
            },
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    fn window_clients(
        &self,
        cte: &str,
        user: &Option<String>,
        start: i64,
        end: i64,
        tz_secs: i64,
    ) -> Result<Vec<WatchClient>, StoreError> {
        let sql = format!(
            "{cte} SELECT COALESCE(client, '直连'), COUNT(*), COALESCE(SUM(watched_ms), 0)
             FROM scoped GROUP BY 1 ORDER BY 1"
        );
        let mut stmt = self.subscribe.prepare(&sql)?;
        let rows = stmt.query_map(
            named_params! { ":tz": tz_secs, ":start": start, ":end": end, ":user": user },
            |row| {
                Ok(WatchClient {
                    client: row.get(0)?,
                    plays: row.get(1)?,
                    watched_ms: row.get(2)?,
                })
            },
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    fn window_media(
        &self,
        cte: &str,
        user: &Option<String>,
        start: i64,
        end: i64,
        tz_secs: i64,
    ) -> Result<Vec<WatchMedia>, StoreError> {
        let sql = format!(
            "{cte} SELECT media_id, COUNT(*), COALESCE(SUM(watched_ms), 0),
                    COUNT(DISTINCT user_id)
             FROM scoped GROUP BY media_id ORDER BY media_id"
        );
        let mut stmt = self.subscribe.prepare(&sql)?;
        let rows = stmt.query_map(
            named_params! { ":tz": tz_secs, ":start": start, ":end": end, ":user": user },
            |row| {
                Ok(WatchMedia {
                    media_id: media_id_from(row, 0)?,
                    plays: row.get(1)?,
                    watched_ms: row.get(2)?,
                    members: row.get(3)?,
                })
            },
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// TEMP 可见集合：清空 + 建表（TEMP 表随连接存在，故每次调用都要清）。
    fn reset_watch_visible_media(&self) -> Result<(), StoreError> {
        self.subscribe.execute_batch(
            "CREATE TEMP TABLE IF NOT EXISTS request_visible_media (id TEXT PRIMARY KEY);
             DELETE FROM request_visible_media;",
        )?;
        Ok(())
    }

    /// 批量填充可见集合：只有 `?` 占位符，用户数据不进 SQL 文本。
    fn fill_watch_visible_media(&self, ids: &[MediaId]) -> Result<(), StoreError> {
        for chunk in ids.chunks(VISIBLE_CHUNK) {
            let placeholders = vec!["(?)"; chunk.len()].join(",");
            let sql =
                format!("INSERT OR IGNORE INTO request_visible_media (id) VALUES {placeholders}");
            self.subscribe.execute(
                &sql,
                rusqlite::params_from_iter(chunk.iter().map(MediaId::to_string)),
            )?;
        }
        Ok(())
    }
}
