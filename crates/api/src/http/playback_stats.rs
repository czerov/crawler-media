//! Playback statistics assembled from the Store's SQL aggregation.
//!
//! 之前这里做两次 `list_logs(200_000, ..)` 全量加载再在 Rust 里过滤可见性/分组；
//! 现在改成 `Store::playback_watch_report` 一次 SQL 聚合。DTO 字段与既有语义不变。

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use std::collections::HashMap;

use crate::http::playback::media_target_json;
use crate::http::playback_logs::{member_filter, member_name, visible_media_ids};
use crate::http::{err, ok};
use crate::job_loop::unix_now;
use crate::management::ApiState;
use crate::store::{WatchDay, WatchMedia, WatchReport, WatchTotals};

/// 时区偏移（分钟）的合法范围：±24h。
const MAX_TZ_MINUTES: i64 = 24 * 60;

/// GET /playback/stats/watch?days&tz_offset&member_id&scope
pub(crate) async fn watch_stats(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let days = query
        .get("days")
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(30)
        .clamp(1, 365);
    let tz_minutes = query
        .get("tz_offset")
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0);
    if !(-MAX_TZ_MINUTES..=MAX_TZ_MINUTES).contains(&tz_minutes) {
        return err(
            StatusCode::BAD_REQUEST,
            "playback.invalid",
            "tz_offset 超出 ±1440 分钟",
        );
    }
    let now = unix_now();
    let store = state.store.lock();
    let admin = store
        .user_role(user_id)
        .map(|role| role == "admin")
        .unwrap_or(false);
    let target_user = if admin {
        member_filter(&store, query.get("member_id").map(String::as_str))
    } else {
        Some(user_id)
    };
    let scope = if admin && query.get("scope").is_some_and(|scope| scope == "all") {
        "all"
    } else {
        "visible"
    };
    let visible: Option<Vec<domain::MediaId>> =
        visible_media_ids(&store, user_id, scope).map(|ids| ids.into_iter().collect());
    let report = match store.playback_watch_report(
        target_user,
        visible.as_deref(),
        now,
        days as u16,
        tz_minutes as i16,
    ) {
        Ok(report) => report,
        Err(error) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            );
        }
    };
    ok(stats_json(&store, &report, days)).into_response()
}

fn totals_json(totals: &WatchTotals) -> Value {
    json!({
        "plays": totals.plays,
        "watched_ms": totals.watched_ms,
        "completed": totals.completed,
        "active_members": totals.members,
    })
}

fn day_rows(days: &[WatchDay]) -> Vec<Value> {
    days.iter()
        .map(|entry| {
            json!({
                "date": iso_date(entry.day * 86_400),
                "plays": entry.totals.plays,
                "watched_ms": entry.totals.watched_ms,
                "completed": entry.totals.completed,
                "members": entry.totals.members,
            })
        })
        .collect()
}

/// `by_members = false` → 按 watched_ms 降序（并列看 members）；否则按 members
/// 降序（并列看 watched_ms）。与既有排序/并列规则一致。
fn media_rows(
    store: &crate::Store,
    media: &[WatchMedia],
    by_members: bool,
    limit: usize,
) -> Vec<Value> {
    let mut rows: Vec<&WatchMedia> = media.iter().collect();
    rows.sort_by_key(|entry| {
        std::cmp::Reverse(if by_members {
            (entry.members, entry.watched_ms)
        } else {
            (entry.watched_ms, entry.members)
        })
    });
    rows.into_iter()
        .take(limit)
        .map(|entry| {
            json!({
                "media": media_target_json(store, entry.media_id, None, None),
                "plays": entry.plays,
                "watched_ms": entry.watched_ms,
                "members": entry.members,
            })
        })
        .collect()
}

fn stats_json(store: &crate::Store, report: &WatchReport, days: i64) -> Value {
    let current = &report.current;
    let previous = &report.previous;
    let member_rows: Vec<Value> = current
        .users
        .iter()
        .map(|member| {
            json!({
                "member_id": member.user_id.to_string(),
                "member_name": member_name(store, member.user_id),
                "plays": member.totals.plays,
                "watched_ms": member.totals.watched_ms,
                "completed": member.totals.completed,
            })
        })
        .collect();
    let client_rows: Vec<Value> = current
        .clients
        .iter()
        .map(|client| {
            json!({
                "client": client.client,
                "plays": client.plays,
                "watched_ms": client.watched_ms,
            })
        })
        .collect();
    json!({
        "days": days,
        "current": totals_json(&current.totals),
        "previous": totals_json(&previous.totals),
        "previous_available": previous.totals.plays > 0,
        "by_day": day_rows(&current.days),
        "previous_by_day": day_rows(&previous.days),
        "by_hour": current.hours,
        "by_member": member_rows,
        "by_client": client_rows,
        "by_tier": [ { "tier": 0, "label": "直连", "plays": current.totals.plays } ],
        "top_titles": media_rows(store, &current.media, false, 8),
        "hidden_title_count": report.hidden_titles,
        "favorites": media_rows(store, &current.media, true, 3),
        "previous_favorites": media_rows(store, &previous.media, true, 3),
    })
}

fn iso_date(unix_secs: i64) -> String {
    // 按浏览器时区偏移后的日期（YYYY-MM-DD），不做绝对时间换算。
    let days = unix_secs.div_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Days → (year, month, day) in the shifted timezone (Howard Hinnant's algo).
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}
