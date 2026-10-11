//! Auth: login returns the user token (ADR-0003 plaintext token) and an
//! HttpOnly same-origin cookie for browser requests such as `<img>` loads.
//! Protected routes accept either the cookie or `Authorization: Bearer <token>`.
//! 管理员授权是独立的一层：`authenticate` 只解析身份，`require_admin`
//! 中间件在 admin Router 上统一 403，避免逐 handler 漏加。

use axum::Json;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::http::{err, ok};
use crate::management::ApiState;

const SESSION_COOKIE: &str = "mc_session";

pub(crate) async fn login(State(state): State<ApiState>, Json(body): Json<Value>) -> Response {
    let username = body["username"].as_str().unwrap_or_default();
    let password = body["password"].as_str().unwrap_or_default();
    if username.is_empty() || password.is_empty() {
        return err(
            StatusCode::BAD_REQUEST,
            "auth.bad_request",
            "用户名和密码必填",
        );
    }
    let authenticated = state
        .password_login
        .authenticate(state.store(), username.to_string(), password.to_string())
        .await;
    let (token, user) = match authenticated {
        Ok(Some(authenticated)) => authenticated,
        Ok(None) => {
            tracing::warn!(username = %username, "登录失败：用户名或密码不正确");
            return err(
                StatusCode::UNAUTHORIZED,
                "auth.invalid",
                "用户名或密码不正确",
            );
        }
        Err(crate::password_login::LoginError::Busy) => {
            tracing::warn!(username = %username, "登录并发已达上限，拒绝请求");
            let mut response = err(
                StatusCode::SERVICE_UNAVAILABLE,
                "auth.busy",
                "登录服务繁忙，请稍后重试",
            );
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
            return response;
        }
        Err(error) => {
            // DB/worker/hash 故障绝不能被表述成“密码错误”，也不签发 token。
            tracing::error!(username = %username, error = %error, "登录服务内部错误");
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "auth.error",
                "登录服务暂时不可用",
            );
        }
    };
    tracing::info!(user_id = %user.id, login = %user.login, role = user.role.as_str(), "用户登录成功");
    let mut payload = ok(json!({
        "token": token.clone(),
        "user": {
            "id": user.id.to_string(),
            "login": user.login,
            "role": user.role.as_str(),
            "enabled": user.enabled,
        },
    }))
    .into_response();
    let cookie = format!("{SESSION_COOKIE}={token}; Path=/; HttpOnly; SameSite=Lax");
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        payload.headers_mut().append(header::SET_COOKIE, value);
    }
    payload
}

/// POST /auth/logout — 撤销当前会话 token（Header 或 cookie 之一命中即撤销），
/// 并清除浏览器会话 cookie。幂等：无 token / 未知 token 也返回成功。
pub(crate) async fn logout(State(state): State<ApiState>, request: Request) -> Response {
    let mut tokens = Vec::new();
    if let Some(bearer) = bearer_token(&request) {
        tokens.push(bearer);
    }
    if let Some(cookie) = cookie_token(&request) {
        tokens.push(cookie);
    }
    tokens.sort();
    tokens.dedup();

    if let Err(error) = state.store.lock().delete_session_tokens(&tokens) {
        tracing::error!(%error, "撤销会话失败");
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        );
    }

    let mut response = ok(Value::Null).into_response();
    let clear = format!("{SESSION_COOKIE}=; Path=/; Max-Age=0; HttpOnly; SameSite=Lax");
    if let Ok(value) = HeaderValue::from_str(&clear) {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
    response
}

fn bearer_token(request: &Request) -> Option<String> {
    request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_string)
}

fn query_token(request: &Request) -> Option<String> {
    let path = request.uri().path();
    // 窄范围 query-token：仅允许视频取流、字幕取流和直读类播放资源路由
    let is_playback_resource = path.starts_with("/api/v1/playback/subtitles/")
        || path.starts_with("/Videos/")
        || path.starts_with("/playback/subtitles/");
    if !is_playback_resource {
        return None;
    }
    request.uri().query().and_then(|query| {
        query.split('&').find_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            (key.eq_ignore_ascii_case("api_key")
                || key.eq_ignore_ascii_case("apikey")
                || key.eq_ignore_ascii_case("token"))
            .then(|| value.to_string())
        })
    })
}

/// 从请求中提取会话 token：优先 Authorization: Bearer，其次会话 cookie，取流端点允许 query token。
fn request_token(request: &Request) -> Option<String> {
    bearer_token(request)
        .or_else(|| cookie_token(request))
        .or_else(|| query_token(request))
}

fn cookie_token(request: &Request) -> Option<String> {
    request
        .headers()
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|cookies| {
            cookies.split(';').find_map(|pair| {
                let pair = pair.trim();
                pair.strip_prefix(SESSION_COOKIE)?.strip_prefix('=')
            })
        })
        .filter(|t| !t.is_empty())
        .map(str::to_string)
}

/// Resolve the acting user from the bearer token (or session cookie).
pub(crate) async fn authenticate(
    State(store): State<std::sync::Arc<parking_lot::Mutex<crate::Store>>>,
    mut request: Request,
    next: Next,
) -> Response {
    let Some(token) = request_token(&request) else {
        return err(
            StatusCode::UNAUTHORIZED,
            "auth.required",
            "未登录或会话已过期",
        );
    };
    let Some(user_id) = store.lock().user_id_by_token(&token).ok().flatten() else {
        return err(
            StatusCode::UNAUTHORIZED,
            "auth.required",
            "未登录或会话已过期",
        );
    };
    request.extensions_mut().insert(user_id);
    request.extensions_mut().insert(token);
    // 可选身份提取：库浏览等端点对“匿名可读”与“成员可见范围”共用同一 handler。
    request.extensions_mut().insert(Some(user_id));
    next.run(request).await
}

/// 管理员授权层：必须挂在 `authenticate` 之后（内层），读取其注入的身份。
/// 对两套路由通吃：/api/v1 注入 `UserId`，legacy/Jellyfin 注入 `AuthUser`。
pub(crate) async fn require_admin(
    State(store): State<std::sync::Arc<parking_lot::Mutex<crate::Store>>>,
    request: Request,
    next: Next,
) -> Response {
    let user_id = request
        .extensions()
        .get::<crate::auth::AuthUser>()
        .map(|user| user.id)
        .or_else(|| request.extensions().get::<domain::UserId>().copied());
    let Some(user_id) = user_id else {
        return err(
            StatusCode::UNAUTHORIZED,
            "auth.required",
            "未登录或会话已过期",
        );
    };
    let admin = store
        .lock()
        .user_role(user_id)
        .map(|role| role == "admin")
        .unwrap_or(false);
    if admin {
        next.run(request).await
    } else {
        err(
            StatusCode::FORBIDDEN,
            "auth.forbidden",
            "只有管理员可以执行该操作",
        )
    }
}

pub(crate) async fn me(
    State(state): State<ApiState>,
    axum::Extension(user_id): axum::Extension<domain::UserId>,
) -> Response {
    let store = state.store.lock();
    let Some(user) = store.get_user(user_id).ok().flatten() else {
        return err(StatusCode::UNAUTHORIZED, "auth.required", "用户不存在");
    };
    ok(json!({
        "id": user.id.to_string(),
        "login": user.login,
        "role": user.role.as_str(),
        "enabled": user.enabled,
    }))
    .into_response()
}
