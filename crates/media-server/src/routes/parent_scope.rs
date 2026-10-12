//! Jellyfin `ParentId` classification and user-path validation.
//!
//! `resolve_parent_scope` 只做**不需要读 items** 的分类（Root / Library / Series /
//! Folder），认不出来的一律返回 `Unknown`。Genre / Studio 只能靠 item metadata
//! 判断，由调用方用**已经取到的** items 调 [`classify_genre_or_studio`] 升级——
//! 这样分类不会为了判断 genre 再整库读一次。

use std::path::PathBuf;

use axum::http::StatusCode;
use domain::UserId;

use crate::provider::{MediaItemSnapshot, MediaServerProvider, ServerLibrary};

pub(super) enum ParentScope {
    Root,
    Library(String),
    Series(String),
    Folder(PathBuf),
    Genre(String),
    Studio(String),
    Unknown,
}

/// `folder_<uuid5(root)>` 对应的库根目录。
fn folder_root(parent: &str, libraries: &[ServerLibrary]) -> Option<PathBuf> {
    libraries.iter().find_map(|library| {
        library.root_paths.iter().find_map(|root| {
            let id = format!(
                "folder_{}",
                uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, root.to_string_lossy().as_bytes())
                    .simple()
            );
            (id == parent).then(|| root.clone())
        })
    })
}

/// 该 parent 是否就是某个可见库的 id（廉价判断，不读 items）。
pub(super) fn library_parent<'a>(
    parent: Option<&'a str>,
    libraries: &[ServerLibrary],
) -> Option<&'a str> {
    let parent = parent?;
    libraries
        .iter()
        .any(|library| library.id == parent)
        .then_some(parent)
}

/// 分类 parent：只有完全不带 parent/series 才是 `Root`；认不出来是 `Unknown`。
///
/// 优先级与既有 `items` 实现一致：library > series > folder。
pub(super) async fn resolve_parent_scope(
    provider: &dyn MediaServerProvider,
    user: UserId,
    parent: Option<&str>,
    series: Option<&str>,
    libraries: &[ServerLibrary],
) -> Result<ParentScope, StatusCode> {
    let is_folder = parent
        .map(|value| value.starts_with("folder_"))
        .unwrap_or(false);
    let is_library = library_parent(parent, libraries).is_some();
    let is_series = series.is_some()
        || (!is_library
            && !is_folder
            && match parent {
                Some(id) => provider
                    .resolve_single_item(user, id)
                    .await
                    .ok()
                    .flatten()
                    .map(|item| item.is_series)
                    .unwrap_or(false),
                None => false,
            });
    if is_library {
        return Ok(ParentScope::Library(parent.unwrap_or_default().to_string()));
    }
    if is_series {
        // 与既有 `parent.or(series)` 一致：parent 存在时优先。
        let id = parent.or(series).unwrap_or_default().to_string();
        return Ok(ParentScope::Series(id));
    }
    if is_folder {
        return Ok(folder_root(parent.unwrap_or_default(), libraries)
            .map(ParentScope::Folder)
            .unwrap_or(ParentScope::Unknown));
    }
    if parent.is_none() {
        return Ok(ParentScope::Root);
    }
    Ok(ParentScope::Unknown)
}

/// 用可见 items 判断 parent 是否指向某个 genre/studio（名字，或 uuid5 的连字符 /
/// 紧凑形式）。两者都不是则 `Unknown`——调用方必须返回空集合，不能退回整库。
pub(super) fn classify_genre_or_studio(parent: &str, items: &[MediaItemSnapshot]) -> ParentScope {
    let matches = |value: &str| {
        let hyphenated = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, value.as_bytes())
            .hyphenated()
            .to_string();
        let simple = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, value.as_bytes())
            .simple()
            .to_string();
        parent.eq_ignore_ascii_case(&hyphenated)
            || parent.eq_ignore_ascii_case(&simple)
            || value.eq_ignore_ascii_case(parent)
    };
    let mut genre = false;
    let mut studio = false;
    for item in items {
        genre |= item.metadata.genres.iter().any(|value| matches(value));
        studio |= item.metadata.studios.iter().any(|value| matches(value));
    }
    if genre {
        ParentScope::Genre(parent.to_string())
    } else if studio {
        ParentScope::Studio(parent.to_string())
    } else {
        ParentScope::Unknown
    }
}

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
