//! G-M4: a full-season-pack subscription may fall back to single episodes
//! once it has waited `subscribe.pack_fallback_after:<id>` days without a pack.
//! Unset (default) keeps the original "pack only" behaviour.

use std::borrow::Cow;

use domain::{Subscribe, SubscribeId};

use crate::management::ApiState;

const DAY_SECS: i64 = 86_400;

pub(crate) fn after_key(id: SubscribeId) -> String {
    format!("subscribe.pack_fallback_after:{id}")
}

pub(crate) fn since_key(id: SubscribeId) -> String {
    format!("subscribe.pack_wait_since:{id}")
}

/// Store (or clear with `None`) the fallback delay in days.
pub(crate) fn set_after_days(state: &ApiState, id: SubscribeId, days: Option<u32>) {
    let store = state.store.lock();
    let result = match days {
        Some(days) => store.put_setting(&after_key(id), &days.to_string()),
        None => store.delete_setting(&after_key(id)).map(|_| ()),
    };
    if let Err(error) = result {
        tracing::warn!(subscribe_id = %id, %error, "保存 pack_fallback_after 失败");
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// The subscription the chooser should see this round: unchanged, or with
/// `full_season_pack` off once the wait has exceeded the configured delay.
/// Takes the store lock briefly; the wait starts at the first evaluation.
pub(crate) fn effective<'a>(state: &ApiState, subscribe: &'a Subscribe) -> Cow<'a, Subscribe> {
    if !subscribe.full_season_pack {
        return Cow::Borrowed(subscribe);
    }
    let store = state.store.lock();
    let Some(days) = store
        .get_setting(&after_key(subscribe.id))
        .ok()
        .flatten()
        .and_then(|value| value.trim().parse::<i64>().ok())
    else {
        return Cow::Borrowed(subscribe);
    };
    let now = now_secs();
    let since = match store
        .get_setting(&since_key(subscribe.id))
        .ok()
        .flatten()
        .and_then(|value| value.trim().parse::<i64>().ok())
    {
        Some(since) => since,
        None => {
            let _ = store.put_setting(&since_key(subscribe.id), &now.to_string());
            now
        }
    };
    if now.saturating_sub(since) < days.saturating_mul(DAY_SECS) {
        return Cow::Borrowed(subscribe);
    }
    tracing::info!(subscribe_id = %subscribe.id, days, "整季包等待超时，回退按集选择");
    let mut fallback = subscribe.clone();
    fallback.full_season_pack = false;
    Cow::Owned(fallback)
}
