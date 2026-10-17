use domain::{Confidence, Release};

mod attributes;
mod boundary;
use attributes::*;
use boundary::*;

pub fn parse(raw: &str) -> Release {
    // 分词（优先从原始标题提取语言等属性，避免段首 [中字] 等标签被提前剥离导致丢失）
    let raw_tokens = tokenize(raw);
    let subtitle_language = raw_tokens.iter().find_map(|t| parse_subtitle_language(t));
    let audio_language = raw_tokens.iter().find_map(|t| parse_audio_language(t));

    // 0. 借鉴 MovieClaw / MoviePilot：剥离段首发布标签，如 [中字]、[肥猫发布] 等
    let clean_raw = strip_leading_brackets(raw);

    // 1. 优先使用基于整串上下文的季集和标题切分（处理 S01E01、S01 E01-E04、S01、Season 1、第x季第x集等）
    let (boundary_title, boundary_season, boundary_ep, boundary_ep_to) =
        extract_boundary_season_episode(clean_raw);

    // 分词时注意保留像 H.264 中的点，但切分常见的间隔符
    // 遇到包含字母和点混杂（例如 H.264）的不应该盲目按点拆散
    let tokens = tokenize(clean_raw);

    let year = tokens.iter().find_map(|t| parse_year(t));
    let resolution = tokens.iter().find_map(|t| parse_resolution(t));
    // 遇到包含 REMUX 和 BluRay 组合的情况，REMUX 拥有更高片源优先级
    let source = if tokens.iter().any(|t| t.eq_ignore_ascii_case("remux")) {
        Some("Remux".to_string())
    } else {
        tokens.iter().find_map(|t| parse_source(t))
    };
    let codec = tokens.iter().find_map(|t| parse_codec(t));
    let hdr = tokens.iter().find_map(|t| parse_hdr(t));

    // 2. 如果 boundary 没匹配出季或集，进行多 token 流状态机扫描（对齐 MoviePilot MetaVideo）
    let (season, episode, episode_to) = if boundary_season.is_some() || boundary_ep.is_some() {
        (boundary_season, boundary_ep, boundary_ep_to)
    } else {
        scan_tokens_for_season_episode(&tokens)
    };

    let group = raw.rsplit_once('-').map(|(_, g)| g.trim().to_string());

    let title = if let Some(bt) = boundary_title {
        bt
    } else {
        let mut title_tokens = Vec::new();
        for token in &tokens {
            if token == "-" {
                continue;
            }
            if parse_year(token).is_some()
                || parse_resolution(token).is_some()
                || parse_source(token).is_some()
                || parse_codec(token).is_some()
                || parse_hdr(token).is_some()
                || is_season_or_episode_token(token)
            {
                break;
            }
            title_tokens.push(token.as_str());
        }
        if title_tokens.is_empty() {
            raw.to_string()
        } else {
            title_tokens.join(" ")
        }
    };

    let confidence = if year.is_some()
        || resolution.is_some()
        || source.is_some()
        || season.is_some()
        || episode.is_some()
    {
        Confidence::High
    } else {
        Confidence::Low
    };

    let release = Release {
        title,
        year,
        season,
        episode,
        episode_to,
        resolution,
        source,
        codec,
        hdr,
        subtitle_language,
        audio_language,
        group,
        confidence,
        season_inferred: season.is_some() && !has_explicit_season(clean_raw),
    };
    tracing::debug!(
        raw = %raw,
        title = %release.title,
        season = ?release.season,
        episode = ?release.episode,
        resolution = ?release.resolution,
        source = ?release.source,
        codec = ?release.codec,
        confidence = ?release.confidence,
        "解析发布名"
    );
    release
}

/// True when `raw` names a season itself (`S01`, `S1E02`, `Season 2`, `第二季`).
pub fn has_explicit_season(raw: &str) -> bool {
    if raw.contains('季') {
        return true;
    }
    let upper = raw.to_ascii_uppercase();
    let bytes = upper.as_bytes();
    (0..bytes.len()).any(|i| {
        let at_word = i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
        at_word
            && bytes[i] == b'S'
            && (bytes.get(i + 1).is_some_and(u8::is_ascii_digit) || upper[i..].starts_with("SEASON"))
    })
}

fn tokenize(raw: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut cur = String::new();
    let chars: Vec<char> = raw.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        let c = chars[i];
        if c == ' '
            || c == '_'
            || c == '['
            || c == ']'
            || c == '('
            || c == ')'
            || c == '【'
            || c == '】'
        {
            if !cur.is_empty() {
                tokens.push(std::mem::take(&mut cur));
            }
        } else if c == '.' {
            // 如果是 H.264 这种特定格式，保留点
            let is_h264 = cur.eq_ignore_ascii_case("H")
                && i + 3 < len
                && chars[i + 1] == '2'
                && chars[i + 2] == '6'
                && (chars[i + 3] == '4' || chars[i + 3] == '5');
            if is_h264 {
                cur.push(c);
            } else {
                if !cur.is_empty() {
                    tokens.push(std::mem::take(&mut cur));
                }
            }
        } else {
            cur.push(c);
        }
        i += 1;
    }
    if !cur.is_empty() {
        tokens.push(cur);
    }
    tokens
}

fn is_season_or_episode_token(token: &str) -> bool {
    let upper = token.to_ascii_uppercase();
    if upper.starts_with('S') && upper[1..].chars().all(|c| c.is_ascii_digit()) {
        return true;
    }
    if upper.starts_with('E') && upper[1..].chars().any(|c| c.is_ascii_digit()) {
        return true;
    }
    if upper == "SEASON" || upper == "EPISODE" {
        return true;
    }
    false
}

/// 模拟 MoviePilot 的 token 流扫描：支持独立 S01 + E01-E04、Season 1、E01 等
fn scan_tokens_for_season_episode(tokens: &[String]) -> (Option<u32>, Option<u32>, Option<u32>) {
    let mut season: Option<u32> = None;
    let mut episode: Option<u32> = None;
    let mut episode_to: Option<u32> = None;
    let mut last_was_season_word = false;
    let mut last_was_episode_word = false;

    for token in tokens {
        let upper = token.to_ascii_uppercase();

        // 1. Season xx
        if last_was_season_word {
            last_was_season_word = false;
            if let Ok(s) = token.parse::<u32>() {
                if season.is_none() && s < 100 {
                    season = Some(s);
                    continue;
                }
            }
        }
        if upper == "SEASON" {
            last_was_season_word = true;
            continue;
        }

        // 2. Episode xx
        if last_was_episode_word {
            last_was_episode_word = false;
            if plausible_episode(token) {
                if let Ok(e) = token.parse::<u32>() {
                    if episode.is_none() {
                        episode = Some(e);
                        if season.is_none() {
                            season = Some(1);
                        }
                        continue;
                    }
                }
            }
        }
        if upper == "EPISODE" {
            last_was_episode_word = true;
            continue;
        }

        // 3. S01E01 或 S01E01-E04
        if upper.starts_with('S') {
            if let Some(e_pos) = upper.find('E') {
                if e_pos > 1 {
                    let s_str = &upper[1..e_pos];
                    if let Ok(s) = s_str.parse::<u32>() {
                        if season.is_none() {
                            season = Some(s);
                        }
                        let ep_part = &upper[e_pos..];
                        let (from, to) = parse_ep_numbers(ep_part);
                        if from.is_some() {
                            episode = from;
                            episode_to = to;
                            break;
                        }
                    }
                }
            } else {
                // S01 (单季无集)
                let s_str = &upper[1..];
                if !s_str.is_empty() && s_str.chars().all(|c| c.is_ascii_digit()) {
                    if let Ok(s) = s_str.parse::<u32>() {
                        if season.is_none() && s < 100 {
                            season = Some(s);
                            continue;
                        }
                    }
                }
            }
        }

        // 4. E01-E04 或 EP01
        if (upper.starts_with('E')
            && upper.len() > 1
            && upper[1..].chars().any(|c| c.is_ascii_digit()))
            || (upper.starts_with("EP")
                && upper.len() > 2
                && upper[2..].chars().any(|c| c.is_ascii_digit()))
        {
            let ep_part = if upper.starts_with("EP") {
                &upper[1..]
            } else {
                &upper[..]
            };
            let (from, to) = parse_ep_numbers(ep_part);
            if from.is_some() {
                if episode.is_none() {
                    episode = from;
                    episode_to = to;
                    if season.is_none() {
                        season = Some(1);
                    }
                }
                break;
            }
        }
    }

    (season, episode, episode_to)
}

fn parse_ep_numbers(ep_part: &str) -> (Option<u32>, Option<u32>) {
    let digits: Vec<&str> = ep_part
        .split(|c: char| !c.is_ascii_digit())
        .filter(|text| plausible_episode(text))
        .collect();
    if digits.is_empty() {
        if let Some(token) = ep_part.split(|c: char| !c.is_ascii_digit()).find(|text| {
            text.len() == 8 && boundary::is_calendar_date_text(text)
        }) {
            boundary::reject_date_token(token);
        }
    }
    match digits.as_slice() {
        [from] => (from.parse().ok(), None),
        [from, to] => {
            let f = from.parse().ok();
            let t = to.parse().ok();
            (
                f,
                t.filter(|to_val| f.is_some_and(|f_val| *to_val >= f_val)),
            )
        }
        _ => (None, None),
    }
}

#[cfg(test)]
mod season_inferred_tests {
    #[test]
    fn missing_season_marker_is_flagged_as_inferred() {
        let r = super::parse("Show Episode 13 1080p WEB-DL");
        assert_eq!((r.season, r.episode, r.season_inferred), (Some(1), Some(13), true));
        assert!(super::parse("Show E05 1080p").season_inferred);
        assert!(!super::parse("Show.S01E05.1080p").season_inferred);
        assert!(!super::parse("Show Season 2 Episode 3").season_inferred);
        assert!(!super::parse("剧名 第二季 第03集").season_inferred);
        assert!(!super::parse("Movie.2020.1080p").season_inferred);
    }
}
