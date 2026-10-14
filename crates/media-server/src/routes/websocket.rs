use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Extension, Query, State};
use axum::response::IntoResponse;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tokio::sync::broadcast::{self, error::RecvError};
use tokio::time::{self, Duration, MissedTickBehavior};
use uuid::Uuid;

use super::AppState;
use crate::AuthUser;

#[derive(Default, Deserialize)]
pub(super) struct SocketQuery {
    #[serde(default, rename = "deviceId", alias = "DeviceId")]
    device_id: Option<String>,
}

pub(super) async fn connect(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    Query(query): Query<SocketQuery>,
    upgrade: WebSocketUpgrade,
) -> impl IntoResponse {
    let receiver = state.data_changes.subscribe();
    let provider = state.provider.clone();
    upgrade
        .on_upgrade(move |socket| run_socket(socket, user, receiver, query.device_id, provider))
        .into_response()
}

async fn send_with_timeout(socket: &mut WebSocket, msg: Message) -> bool {
    matches!(
        tokio::time::timeout(Duration::from_secs(5), socket.send(msg)).await,
        Ok(Ok(()))
    )
}

async fn run_socket(
    mut socket: WebSocket,
    user: AuthUser,
    mut changes: broadcast::Receiver<(domain::UserId, Value)>,
    device_id: Option<String>,
    provider: std::sync::Arc<dyn crate::provider::MediaServerProvider>,
) {
    let user_id = user.id;
    tracing::info!(%user_id, ?device_id, "Jellyfin WebSocket client connected");
    let mut keepalive = time::interval(Duration::from_secs(30));
    keepalive.set_missed_tick_behavior(MissedTickBehavior::Delay);
    keepalive.tick().await;

    loop {
        tokio::select! {
            changed = changes.recv() => match changed {
                Ok((event_user, event)) if event_user == user_id => {
                    // 检查 token 是否仍有效（logout 或改密后断开）
                    if provider.user_id_by_token(&user.token).await.ok().flatten() != Some(user_id) {
                        tracing::info!(%user_id, "Token revoked or password changed, closing WebSocket");
                        break;
                    }
                    if !send_with_timeout(&mut socket, Message::Text(event.to_string().into())).await {
                        break;
                    }
                }
                Ok(_) => {}
                Err(RecvError::Lagged(skipped)) => {
                    tracing::warn!(%user_id, skipped, "Jellyfin WebSocket client missed user data events");
                }
                Err(RecvError::Closed) => break,
            },
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Text(text))) if is_keepalive(&text) => {
                    if !send_with_timeout(&mut socket, Message::Text(json!({"MessageType": "KeepAlive"}).to_string().into())).await {
                        break;
                    }
                }
                Some(Ok(Message::Ping(payload))) => {
                    if !send_with_timeout(&mut socket, Message::Pong(payload)).await {
                        break;
                    }
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => {}
            },
            _ = keepalive.tick() => {
                if provider.user_id_by_token(&user.token).await.ok().flatten() != Some(user_id) {
                    tracing::info!(%user_id, "Token revoked or password changed on keepalive tick, closing WebSocket");
                    break;
                }
                if !send_with_timeout(&mut socket, Message::Text(json!({"MessageType": "KeepAlive"}).to_string().into())).await {
                    break;
                }
            }
        }
    }
    tracing::info!(%user_id, ?device_id, "Jellyfin WebSocket client disconnected");
}

fn is_keepalive(text: &str) -> bool {
    serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|message| {
            message
                .get("MessageType")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .is_some_and(|kind| kind.eq_ignore_ascii_case("KeepAlive"))
}

pub(super) async fn publish_user_data_changed(
    state: &AppState,
    user_id: domain::UserId,
    item_id: &str,
) {
    let snapshot = match state.provider.resolve_single_item(user_id, item_id).await {
        Ok(Some(snapshot)) => snapshot,
        Ok(None) => {
            tracing::warn!(%user_id, %item_id, "cannot publish Jellyfin user data event for an unresolved item");
            return;
        }
        Err(error) => {
            tracing::warn!(%error, %user_id, %item_id, "cannot resolve Jellyfin user data event item");
            return;
        }
    };

    let mut user_data = Vec::new();
    let item = crate::dto::item_dto_json(snapshot);
    if let Some(entry) = user_data_entry(&item) {
        user_data.push(entry);
    }
    if let Some(parent_id) = item.get("SeriesId").and_then(Value::as_str) {
        let parent = match state.provider.resolve_single_item(user_id, parent_id).await {
            Ok(parent) => parent,
            Err(error) => {
                tracing::warn!(%error, %user_id, %parent_id, "cannot resolve Jellyfin series user data event item");
                None
            }
        };
        if let Some(parent) = parent {
            if let Some(entry) = user_data_entry(&crate::dto::item_dto_json(parent))
                && !user_data
                    .iter()
                    .any(|existing| existing["ItemId"] == entry["ItemId"])
            {
                user_data.push(entry);
            }
        }
    }

    let event = json!({
        "MessageType": "UserDataChanged",
        "MessageId": Uuid::new_v4().simple().to_string(),
        "Data": {
            "UserId": user_id.to_string(),
            "ServerId": state.provider.server_id(),
            "UserDataList": user_data,
        }
    });
    match state.data_changes.send((user_id, event)) {
        Ok(clients) => {
            tracing::debug!(%user_id, %item_id, clients, "published Jellyfin user data event")
        }
        Err(_) => {
            tracing::debug!(%user_id, %item_id, "Jellyfin user data changed without connected WebSocket clients")
        }
    }
}

fn user_data_entry(item: &Value) -> Option<Value> {
    let item_id = item.get("Id")?.clone();
    let mut user_data = item.get("UserData")?.as_object()?.clone();
    user_data.insert("ItemId".into(), item_id.clone());
    user_data.insert("Key".into(), item_id);
    Some(Value::Object(Map::from_iter(user_data)))
}
