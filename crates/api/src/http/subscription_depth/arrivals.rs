//! today-arrivals：先在 Store 锁内取 owned 快照，再在 blocking 预算内做
//! stat + catalog 查询 + 渲染。旧实现全程持锁并在锁内发网络请求，而
//! `TmdbHttp` 需要同一个 Store 读取配置 → 锁重入 → 冷缓存时预测整体消失。

use std::collections::HashMap;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::blocking_budget::BlockingError;
use crate::http::subscriptions::{coverage_json, parse_upload_time};
use crate::http::{err, ok_list};
use crate::management::ApiState;

use super::forecast::forecast_for_unit;

/// 锁内读到的 owned 快照；此后不再需要 Store 锁。
pub(crate) struct ArrivalSnapshot {
    pub(crate) subscribe: domain::Subscribe,
    pub(crate) media: domain::Media,
    pub(crate) pending: Vec<(i32, crate::store::PendingDownload)>,
    pub(crate) facts: subscribe::SubscribeFacts,
    pub(crate) rows: Vec<domain::LedgerRow>,
}

/// 只在持有 Store 锁时读取/Clone；DB 错误向上传播（handler 记 500，不静默空集合）。
pub(crate) fn load_arrivals(
    store: &crate::Store,
    user_id: domain::UserId,
) -> Result<Vec<ArrivalSnapshot>, crate::store::StoreError> {
    // 用户隔离：成员只看自己订阅的到货/预计入库；admin 看全部。
    let admin = store.user_role(user_id)? == "admin";
    let subscribes = if admin {
        store.list_all_subscribes()?
    } else {
        store.list_subscribes_for_user(user_id)?
    };
    let mut rows_by_media: HashMap<domain::MediaId, Vec<domain::LedgerRow>> = HashMap::new();
    for row in store.list_ledger()? {
        rows_by_media.entry(row.media_id).or_default().push(row);
    }
    let mut snapshots = Vec::with_capacity(subscribes.len());
    for subscribe in subscribes {
        let Some(media) = store.get_media(subscribe.media_id)? else {
            continue;
        };
        let pending = store.load_pending(subscribe.id)?;
        let facts = if media.kind == domain::MediaKind::Tv {
            store.load_subscribe_facts(subscribe.id)?
        } else {
            subscribe::SubscribeFacts::default()
        };
        let rows = rows_by_media
            .get(&subscribe.media_id)
            .cloned()
            .unwrap_or_default();
        snapshots.push(ArrivalSnapshot {
            subscribe,
            media,
            pending,
            facts,
            rows,
        });
    }
    Ok(snapshots)
}

/// 锁外计算：stat、catalog 查询与 JSON 渲染。同 (tmdb_id, season) 每请求只查一次。
pub(crate) fn build_arrivals(
    snapshots: Vec<ArrivalSnapshot>,
    catalog: &dyn crate::catalog::Catalog,
    now: i64,
) -> Vec<Value> {
    let day_secs = 86_400i64;
    let mut items = Vec::new();
    let mut episodes_cache: HashMap<(String, u32), Vec<media::SeasonEpisode>> = HashMap::new();
    for snapshot in snapshots {
        let ArrivalSnapshot {
            subscribe,
            media,
            pending,
            facts,
            rows,
        } = snapshot;
        // 在途（grabbing）：有 pending 种子的订阅，status=grabbed。
        for _ in &pending {
            items.push(json!({
                "subscription_id": subscribe.id.to_string(),
                "title": media.title,
                "coverage": coverage_json(&subscribe.coverage),
                "eta": null,
                "status": "grabbed",
                "info_hash": Value::Null,
                "grabbed_at": Value::Null,
            }));
        }
        // 当日入库：ledger 行文件修改时间在 24h 内的订阅，status=imported。
        let imported_today = rows
            .iter()
            .any(|row| modified_within_day(&row.path, now, day_secs));
        if imported_today && pending.is_empty() {
            items.push(json!({
                "subscription_id": subscribe.id.to_string(),
                "title": media.title,
                "coverage": coverage_json(&subscribe.coverage),
                "eta": null,
                "status": "imported",
                "info_hash": Value::Null,
                "grabbed_at": Value::Null,
            }));
        }
        // 预计发布（wanted）：TV 订阅中 air_date 落在 [昨天, +7 天] 且未入库的单元。
        if media.kind != domain::MediaKind::Tv {
            continue;
        }
        let Some(tmdb_id) = media.tmdb_id.as_deref() else {
            continue;
        };
        let season = match subscribe.coverage {
            domain::Coverage::Tv { season, .. } => season,
            _ => continue,
        };
        for episode in episodes_for(catalog, &mut episodes_cache, tmdb_id, season) {
            if facts
                .get(Some(season), Some(episode.episode_number))
                .is_some()
            {
                continue; // 已入库
            }
            let Some(air_date) = episode.air_date.as_deref() else {
                continue;
            };
            let Some(air_unix) = parse_upload_time(air_date) else {
                continue;
            };
            if air_unix < now - day_secs || air_unix > now + 7 * day_secs {
                continue;
            }
            let forecast = forecast_for_unit(&subscribe, &pending, air_date).unwrap_or(Value::Null);
            items.push(json!({
                "subscription_id": subscribe.id.to_string(),
                "title": media.title,
                "coverage": coverage_json(&subscribe.coverage),
                "eta": null,
                "status": "wanted",
                "air_date": air_date,
                "season": season,
                "episode": episode.episode_number,
                "info_hash": Value::Null,
                "grabbed_at": Value::Null,
                "release_forecast": forecast,
            }));
        }
    }
    items
}

/// 部分数据源失败只影响该 (tmdb_id, season)：warn 后跳过，保留其余 items。
fn episodes_for<'a>(
    catalog: &dyn crate::catalog::Catalog,
    cache: &'a mut HashMap<(String, u32), Vec<media::SeasonEpisode>>,
    tmdb_id: &str,
    season: u32,
) -> &'a [media::SeasonEpisode] {
    cache
        .entry((tmdb_id.to_string(), season))
        .or_insert_with(|| match catalog.season_episodes(tmdb_id, season) {
            Ok(episodes) => episodes,
            Err(error) => {
                tracing::warn!(
                    %error,
                    tmdb_id,
                    season,
                    "today-arrivals 无法读取季度集数，跳过该季预测"
                );
                Vec::new()
            }
        })
        .as_slice()
}

fn modified_within_day(path: &str, now: i64, day_secs: i64) -> bool {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .map(|modified| {
            modified
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_secs() as i64)
                .unwrap_or(0)
                >= now - day_secs
        })
        .unwrap_or(false)
}

pub(crate) async fn today_arrivals(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
) -> Response {
    let snapshots = {
        let store = state.store.lock();
        load_arrivals(&store, user_id)
    };
    let snapshots = match snapshots {
        Ok(snapshots) => snapshots,
        Err(error) => {
            tracing::error!(%error, %user_id, "today-arrivals 读取快照失败");
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            );
        }
    };
    let catalog = state.catalog.clone();
    let now = crate::job_loop::unix_now();
    // 30s 是总计算等待预算，不替代 CatalogGet 自身的 HTTP 超时。
    let work = state
        .http_work
        .run(std::time::Duration::from_secs(30), move || {
            build_arrivals(snapshots, catalog.as_ref(), now)
        })
        .await;
    match work {
        Ok(items) => ok_list(items).into_response(),
        Err(BlockingError::Busy) => err(
            StatusCode::SERVICE_UNAVAILABLE,
            "runtime.busy",
            "服务繁忙，请重试",
        ),
        Err(BlockingError::Timeout) => err(
            StatusCode::GATEWAY_TIMEOUT,
            "arrivals.timeout",
            "预测计算超时",
        ),
        Err(error) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "arrivals.worker",
            &error.to_string(),
        ),
    }
}
