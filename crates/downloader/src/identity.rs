use domain::Release;

use crate::{names_match, size_matches};

/// 检查两个发布名所代表的季、集范围与年份是否无冲突。
pub fn release_units_agree(left: &Release, right: &Release) -> bool {
    if left.season != right.season {
        return false;
    }
    if left.episode != right.episode {
        return false;
    }
    if left.episode_to != right.episode_to {
        return false;
    }
    if let (Some(y1), Some(y2)) = (left.year, right.year) {
        if y1 != y2 {
            return false;
        }
    }
    true
}

/// 样本任务排除：避免 Sample 任务被误归入正片。
pub fn has_sample_mismatch(left: &str, right: &str) -> bool {
    let s1 = left.to_ascii_lowercase().contains("sample");
    let s2 = right.to_ascii_lowercase().contains("sample");
    s1 != s2
}

fn is_version_modifier(token: &str) -> bool {
    matches!(
        token.to_ascii_lowercase().as_str(),
        "cut"
            | "dc"
            | "extended"
            | "remastered"
            | "special"
            | "edition"
            | "unrated"
            | "proper"
            | "repack"
            | "imax"
    )
}

fn valid_bilingual_or_version_superset(
    subset: &std::collections::HashSet<String>,
    superset: &std::collections::HashSet<String>,
) -> bool {
    if subset.is_empty() || !subset.is_subset(superset) {
        return false;
    }
    let extra: Vec<&String> = superset.difference(subset).collect();
    let has_non_ascii = extra.iter().any(|tok| tok.chars().any(|c| !c.is_ascii()));
    let all_modifiers = extra.iter().all(|tok| is_version_modifier(tok));
    has_non_ascii || all_modifiers
}

/// 核心片名兼容性判定：电影核心片名归一化若不同，仅允许双语别名（含非 ASCII 中文字符）或版本修饰词，
/// 坚决杜绝 Alien 与 Alien Covenant、Matrix 与 Matrix Reloaded 等同语言续集误匹配。
pub fn core_titles_compatible(left: &Release, right: &Release) -> bool {
    let t1 = crate::normalize_name(&left.title);
    let t2 = crate::normalize_name(&right.title);
    if t1.is_empty() || t2.is_empty() || t1 == t2 {
        return true;
    }
    if left.season.is_none()
        && left.episode.is_none()
        && right.season.is_none()
        && right.episode.is_none()
    {
        let w1 = crate::extract_name_tokens(&left.title);
        let w2 = crate::extract_name_tokens(&right.title);
        return valid_bilingual_or_version_superset(&w1, &w2)
            || valid_bilingual_or_version_superset(&w2, &w1);
    }
    t1.contains(&t2) || t2.contains(&t1)
}

/// 综合判定目标 Torrent 与下载器任务快照是否指向同一个种子：
/// 1. 标题必须安全匹配（季集无冲突、年份无冲突、非 Sample 错配、电影核心片名无冲突）；
/// 2. 体积若已知且大于 0，必须精确匹配。
///
/// # Warning (D-L3)
/// Inferred match for display only; destructive operations must not rely on
/// it (see `names_match`).
pub fn torrent_matches_snapshot(
    title: &str,
    size_bytes: Option<u64>,
    snapshot_name: &str,
    snapshot_size: u64,
) -> bool {
    if has_sample_mismatch(title, snapshot_name) {
        return false;
    }
    let parsed_want = release::parse(title);
    let parsed_got = release::parse(snapshot_name);
    if !release_units_agree(&parsed_want, &parsed_got) {
        return false;
    }
    if !core_titles_compatible(&parsed_want, &parsed_got) {
        return false;
    }
    if !names_match(snapshot_name, title) {
        return false;
    }
    if size_bytes.is_some_and(|n| n > 0) {
        return size_matches(snapshot_size, size_bytes);
    }
    true
}
