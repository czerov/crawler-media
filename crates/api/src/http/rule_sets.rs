//! Rule-sets: named Filter groups of atomic rules (CONTEXT.md **Filter**).

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use domain::{AtomRule, Filter, FilterAtom, FilterId};
use serde::Deserialize;
use serde_json::{Value, json};
use std::str::FromStr;

use crate::http::{err, ok, ok_list};
use crate::management::ApiState;
use crate::settings_keys;

#[derive(Deserialize)]
pub(crate) struct RuleSetInput {
    name: String,
    atoms: Vec<AtomInput>,
    #[serde(default)]
    keep_old_versions: Option<bool>,
}

#[derive(Deserialize)]
pub(crate) struct AtomInput {
    kind: String,
    #[serde(default)]
    value: Option<String>,
    priority: i32,
    /// 黑名单原子：命中即排除（platforms_block / hdr_block / release_group_block）。
    #[serde(default)]
    exclude: bool,
}

fn atom_from_input(input: AtomInput) -> Result<FilterAtom, String> {
    let value = || {
        input
            .value
            .clone()
            .filter(|v| !v.is_empty())
            .ok_or_else(|| format!("{} atom 需要 value", input.kind))
    };
    let rule = match input.kind.as_str() {
        "resolution" => AtomRule::Resolution(value()?),
        "source" => AtomRule::Source(value()?),
        "free" => AtomRule::Free,
        "hr" => AtomRule::Hr,
        "video_codec" => AtomRule::Codec(value()?),
        "title_match" => AtomRule::TitleMatch(value()?),
        "hdr" => AtomRule::Hdr(value()?),
        "size" => parse_size_atom(&input.kind, &input.value)?,
        "min_seeders" => AtomRule::MinSeeders(
            input
                .value
                .as_deref()
                .and_then(|v| v.parse().ok())
                .ok_or_else(|| format!("{} atom 需要数字", input.kind))?,
        ),
        "subtitle_language" => AtomRule::SubtitleLanguage(value()?),
        "audio_language" => AtomRule::AudioLanguage(value()?),
        "site" => AtomRule::Site(value()?),
        "wash_target" => AtomRule::WashTarget(value()?),
        "upgrade_ladder" => {
            let ladder = value()?;
            // L3: only the dimensions the chooser understands are accepted.
            if let Some(bad) = subscribe::unknown_ladder_dimension(&ladder) {
                return Err(format!(
                    "upgrade_ladder 维度 {bad} 无效（可选 {}）",
                    subscribe::LADDER_DIMENSIONS.join("/")
                ));
            }
            AtomRule::UpgradeLadder(ladder)
        }
        // 黑名单原子：命中即排除（平台/制作组标记通常可见于标题）。
        "platforms_block" | "hdr_block" | "release_group_block" => {
            if !input.exclude {
                return Err(format!("{} 是排除原子，必须 exclude=true", input.kind));
            }
            AtomRule::TitleMatch(value()?)
        }
        other => return Err(format!("未知的 Filter atom {other}")),
    };
    Ok(FilterAtom {
        priority: input.priority,
        rule,
        exclude: input.exclude,
    })
}

/// `size` 原子：value 形如 `{min_mb}-{max_mb}`（单边可为空，0 表示不设限）。
fn parse_size_atom(kind: &str, value: &Option<String>) -> Result<AtomRule, String> {
    let raw = value
        .as_deref()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| format!("{kind} atom 需要 value"))?;
    let (min, max) = raw
        .split_once('-')
        .ok_or_else(|| format!("{kind} atom 值须为 {{min}}-{{max}}"))?;
    Ok(AtomRule::Size {
        min_mb: parse_opt(min),
        max_mb: parse_opt(max),
    })
}

fn parse_opt(raw: &str) -> Option<u64> {
    raw.trim().parse().ok().filter(|n| *n > 0)
}

fn atom_json(atom: &FilterAtom) -> Value {
    let (kind, value): (&str, Option<String>) = match &atom.rule {
        AtomRule::Resolution(v) => ("resolution", Some(v.clone())),
        AtomRule::Source(v) => ("source", Some(v.clone())),
        AtomRule::Free => ("free", None),
        AtomRule::Hr => ("hr", None),
        AtomRule::Codec(v) => ("video_codec", Some(v.clone())),
        AtomRule::TitleMatch(v) => ("title_match", Some(v.clone())),
        AtomRule::Hdr(v) => ("hdr", Some(v.clone())),
        AtomRule::Size { min_mb, max_mb } => (
            "size",
            Some(format!(
                "{}-{}",
                min_mb.unwrap_or(0),
                max_mb.map_or(String::new(), |m| m.to_string())
            )),
        ),
        AtomRule::MinSeeders(n) => ("min_seeders", Some(n.to_string())),
        AtomRule::SubtitleLanguage(v) => ("subtitle_language", Some(v.clone())),
        AtomRule::AudioLanguage(v) => ("audio_language", Some(v.clone())),
        AtomRule::Site(v) => ("site", Some(v.clone())),
        AtomRule::WashTarget(v) => ("wash_target", Some(v.clone())),
        AtomRule::UpgradeLadder(v) => ("upgrade_ladder", Some(v.clone())),
    };
    json!({
        "kind": kind,
        "value": value,
        "priority": atom.priority,
        "exclude": atom.exclude,
    })
}

fn rule_set_json(filter: &Filter, is_default: bool) -> Value {
    json!({
        "id": filter.id.to_string(),
        "name": filter.name,
        "is_default": is_default,
        "keep_old_versions": filter.keep_old_versions,
        "atoms": filter.atoms.iter().map(atom_json).collect::<Vec<_>>(),
    })
}

pub(crate) async fn list_rule_sets(State(state): State<ApiState>) -> Response {
    let store = state.store.lock();
    let filters = match store.list_filters() {
        Ok(filters) => filters,
        Err(error) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            );
        }
    };
    let default = store
        .get_setting(settings_keys::DEFAULT_FILTER_ID)
        .ok()
        .flatten()
        .map(|id| FilterId::from_str(&id).ok())
        .flatten();
    ok_list(
        filters
            .iter()
            .map(|f| rule_set_json(f, default == Some(f.id)))
            .collect(),
    )
    .into_response()
}

pub(crate) async fn create_rule_set(
    State(state): State<ApiState>,
    Json(body): Json<RuleSetInput>,
) -> Response {
    if body.name.trim().is_empty() {
        return err(
            StatusCode::BAD_REQUEST,
            "rule_set.invalid",
            "规则组名称必填",
        );
    }
    let atoms = match body
        .atoms
        .into_iter()
        .map(atom_from_input)
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(atoms) if !atoms.is_empty() => atoms,
        Ok(_) => {
            return err(
                StatusCode::BAD_REQUEST,
                "rule_set.invalid",
                "规则组至少需要一个 atom",
            );
        }
        Err(message) => return err(StatusCode::BAD_REQUEST, "rule_set.invalid", &message),
    };
    let filter = Filter {
        id: FilterId::new(),
        name: body.name,
        atoms,
        keep_old_versions: body.keep_old_versions.unwrap_or(false),
    };
    let store = state.store.lock();
    if let Err(error) = store.insert_filter(&filter) {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        );
    }
    (StatusCode::CREATED, ok(rule_set_json(&filter, false))).into_response()
}

pub(crate) async fn get_rule_set(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Response {
    let id = match FilterId::from_str(&id) {
        Ok(id) => id,
        Err(_) => {
            return err(
                StatusCode::BAD_REQUEST,
                "rule_set.invalid",
                "规则组 id 无效",
            );
        }
    };
    let store = state.store.lock();
    let Some(filter) = store.get_filter(id).ok().flatten() else {
        return err(StatusCode::NOT_FOUND, "rule_set.missing", "规则组不存在");
    };
    let is_default = store
        .get_setting(settings_keys::DEFAULT_FILTER_ID)
        .ok()
        .flatten()
        .map(|raw| raw == id.to_string())
        .unwrap_or(false);
    ok(rule_set_json(&filter, is_default)).into_response()
}

pub(crate) async fn patch_rule_set(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    Json(body): Json<RuleSetInput>,
) -> Response {
    let id = match FilterId::from_str(&id) {
        Ok(id) => id,
        Err(_) => {
            return err(
                StatusCode::BAD_REQUEST,
                "rule_set.invalid",
                "规则组 id 无效",
            );
        }
    };
    let atoms = match body
        .atoms
        .into_iter()
        .map(atom_from_input)
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(atoms) if !atoms.is_empty() => atoms,
        Ok(_) => {
            return err(
                StatusCode::BAD_REQUEST,
                "rule_set.invalid",
                "规则组至少需要一个 atom",
            );
        }
        Err(message) => return err(StatusCode::BAD_REQUEST, "rule_set.invalid", &message),
    };
    let store = state.store.lock();
    let existing = match store.get_filter(id) {
        Ok(Some(f)) => f,
        Ok(None) => return err(StatusCode::NOT_FOUND, "rule_set.missing", "规则组不存在"),
        Err(e) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &e.to_string(),
            );
        }
    };
    let filter = Filter {
        id,
        name: body.name,
        atoms,
        keep_old_versions: body.keep_old_versions.unwrap_or(existing.keep_old_versions),
    };
    if let Err(error) = store.save_filter(&filter) {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        );
    }
    let expected = id.to_string();
    let is_default = match store.get_setting(settings_keys::DEFAULT_FILTER_ID) {
        Ok(raw) => raw.as_deref() == Some(expected.as_str()),
        Err(error) => {
            return err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            );
        }
    };
    ok(rule_set_json(&filter, is_default)).into_response()
}

pub(crate) async fn delete_rule_set(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Response {
    let id = match FilterId::from_str(&id) {
        Ok(id) => id,
        Err(_) => {
            return err(
                StatusCode::BAD_REQUEST,
                "rule_set.invalid",
                "规则组 id 无效",
            );
        }
    };
    let store = state.store.lock();
    let is_default = store
        .get_setting(settings_keys::DEFAULT_FILTER_ID)
        .ok()
        .flatten()
        .map(|raw| raw == id.to_string())
        .unwrap_or(false);
    if is_default {
        return err(
            StatusCode::BAD_REQUEST,
            "rule_set.protected",
            "不能删除默认规则组",
        );
    }
    match store.delete_filter(id) {
        Ok(true) => ok(json!({ "deleted": true })).into_response(),
        _ => err(StatusCode::NOT_FOUND, "rule_set.missing", "规则组不存在"),
    }
}

pub(crate) async fn put_default_rule_set(
    State(state): State<ApiState>,
    Json(body): Json<Value>,
) -> Response {
    let raw = body["id"].as_str().unwrap_or_default();
    let id = match FilterId::from_str(raw) {
        Ok(id) => id,
        Err(_) => {
            return err(
                StatusCode::BAD_REQUEST,
                "rule_set.invalid",
                "规则组 id 无效",
            );
        }
    };
    let store = state.store.lock();
    if store.get_filter(id).ok().flatten().is_none() {
        return err(StatusCode::NOT_FOUND, "rule_set.missing", "规则组不存在");
    }
    if let Err(error) = store.put_setting(settings_keys::DEFAULT_FILTER_ID, &id.to_string()) {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store.error",
            &error.to_string(),
        );
    }
    ok(json!({ "default_rule_set_id": id.to_string() })).into_response()
}
