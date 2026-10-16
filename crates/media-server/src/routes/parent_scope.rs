//! Jellyfin user 路径校验。
//!
//! `ParentId` 分类、过滤/排序等纯语义已移到 `crate::provider::query`：provider 的
//! 默认实现与协议路由共用同一份规则，provider 不必依赖 routes 的私有模块。

use axum::http::StatusCode;
use domain::UserId;

use crate::provider::MediaServerProvider;

/// Jellyfin user 路径校验：接受本人 UUID（含去连字符形式）、`me`、以及本人 login；
/// 其他用户 404（不用 403，避免区分「存在但无权」与「不存在」）。
pub(super) async fn require_user_path(
    provider: &dyn MediaServerProvider,
    actual: UserId,
    path_user: Option<&str>,
) -> Result<(), StatusCode> {
    let Some(path_user) = path_user else {
        return Ok(());
    };
    let matches_id = path_user
        .replace('-', "")
        .eq_ignore_ascii_case(&actual.to_string().replace('-', ""));
    if matches_id || path_user.eq_ignore_ascii_case("me") {
        return Ok(());
    }
    let user = provider
        .get_user(actual)
        .await
        .map_err(|error| {
            tracing::error!(%error, user_id = %actual, "failed to resolve Jellyfin user path");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;
    if user.login.eq_ignore_ascii_case(path_user) {
        Ok(())
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}
