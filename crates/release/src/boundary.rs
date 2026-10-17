/// 剥离段首的发布标签，如 [中字] 或 【肥猫发布】
pub(super) fn strip_leading_brackets(s: &str) -> &str {
    let mut cur = s.trim();
    while (cur.starts_with('[') && cur.contains(']'))
        || (cur.starts_with('【') && cur.contains('】'))
    {
        if cur.starts_with('[') {
            if let Some(end) = cur.find(']') {
                cur = cur[end + ']'.len_utf8()..].trim();
            } else {
                break;
            }
        } else if cur.starts_with('【') {
            if let Some(end) = cur.find('】') {
                cur = cur[end + '】'.len_utf8()..].trim();
            } else {
                break;
            }
        } else {
            break;
        }
    }
    cur
}

/// 借鉴 MoviePilot：从完整原始串中定位季集边界标记，并精准切分出前面的干净片名
pub(super) fn extract_boundary_season_episode(
    s: &str,
) -> (Option<String>, Option<u32>, Option<u32>, Option<u32>) {
    let upper = s.to_ascii_uppercase();

    // 格式 1: S01E05、S01 E01-E04、S01-S02、S01 (单独季)
    if let Some((pos, season, ep, ep_to)) = find_s_e_pattern(&upper) {
        let title = normalized_boundary_title(&s[..pos]);
        if !title.is_empty() && (season.is_some() || ep.is_some()) {
            return (Some(title), season, ep, ep_to);
        }
    }

    // 格式 2: 中文 "第一季" / "第 2 季" / "第 5 集" / "第01-04集"
    if let Some((pos, season, ep, ep_to)) = find_chinese_season_episode(s) {
        let title = normalized_boundary_title(&s[..pos]);
        if !title.is_empty() {
            return (Some(title), season, ep, ep_to);
        }
    }

    (None, None, None, None)
}

fn normalized_boundary_title(raw: &str) -> String {
    let mut words: Vec<&str> = raw
        .split(|c: char| matches!(c, '.' | '_' | ' ' | '[' | ']' | '(' | ')'))
        .filter(|word| !word.is_empty() && *word != "-")
        .collect();
    if words
        .last()
        .is_some_and(|word| crate::attributes::parse_year(word).is_some())
    {
        words.pop();
    }
    words.join(" ")
}

/// 集号只接受 1 到 999。8 位 `20YYMMDD` 是日期，即使前面写了 `E` 也不是集。
pub(super) fn plausible_episode(digits: &str) -> bool {
    let Ok(episode) = digits.parse::<u32>() else {
        return false;
    };
    if !(1..=999).contains(&episode) {
        return false;
    }
    if digits.len() == 8 && is_calendar_date(digits) {
        return false;
    }
    true
}

pub(super) fn reject_date_token(token: &str) {
    tracing::warn!(token, "日期格式被拒绝为集号");
}

pub(super) fn is_calendar_date_text(digits: &str) -> bool {
    is_calendar_date(digits)
}

fn is_calendar_date(digits: &str) -> bool {
    let year = &digits[..4];
    let month: u32 = digits[4..6].parse().unwrap_or(0);
    let day: u32 = digits[6..8].parse().unwrap_or(0);
    crate::attributes::parse_year(year).is_some() && (1..=12).contains(&month) && (1..=31).contains(&day)
}

fn is_likely_resolution(bytes: &[u8], ep: u32, m: usize) -> bool {
    matches!(ep, 480 | 576 | 720 | 1080 | 2160 | 4320)
        || (m < bytes.len() && matches!(bytes[m], b'P' | b'p' | b'I' | b'i'))
}

fn try_parse_s_followed(
    upper: &str,
    bytes: &[u8],
    len: usize,
    i: usize,
) -> Option<(usize, Option<u32>, Option<u32>, Option<u32>)> {
    let mut j = i + 1;
    while j < len && bytes[j].is_ascii_digit() {
        j += 1;
    }
    let season = upper[i + 1..j].parse::<u32>().ok()?;
    if season >= 100 {
        return None;
    }
    let mut k = j;
    while k < len && matches!(bytes[k], b' ' | b'.' | b'_' | b'-') {
        k += 1;
    }
    if k < len && (bytes[k] == b'E' || (bytes[k] == b'E' && k + 1 < len && bytes[k + 1] == b'P')) {
        let e_start = if k + 1 < len && bytes[k] == b'E' && bytes[k + 1] == b'P' {
            k + 2
        } else {
            k + 1
        };
        let mut m = e_start;
        while m < len && bytes[m].is_ascii_digit() {
            m += 1;
        }
        if m > e_start && plausible_episode(&upper[e_start..m]) {
            let ep = upper[e_start..m].parse::<u32>().ok()?;
            let (ep_to, _) = parse_episode_range(&upper[m..]);
            return Some((i, Some(season), Some(ep), ep_to));
        }
    }
    if k < len && bytes[k].is_ascii_digit() {
        let mut m = k;
        while m < len && bytes[m].is_ascii_digit() {
            m += 1;
        }
        if plausible_episode(&upper[k..m]) {
            if let Ok(ep) = upper[k..m].parse::<u32>() {
                if !is_likely_resolution(bytes, ep, m) {
                    let (ep_to, _) = parse_episode_range(&upper[m..]);
                    return Some((i, Some(season), Some(ep), ep_to));
                }
            }
        }
    }
    if j == len || !bytes[j].is_ascii_alphanumeric() {
        return Some((i, Some(season), None, None));
    }
    None
}

/// 查找 SxxExx、Sxx Exx、Sxx、Exx
pub(super) fn find_s_e_pattern(
    upper: &str,
) -> Option<(usize, Option<u32>, Option<u32>, Option<u32>)> {
    let bytes = upper.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        let is_start = i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
        if is_start && bytes[i] == b'S' && i + 1 < len && bytes[i + 1].is_ascii_digit() {
            if let Some(res) = try_parse_s_followed(upper, bytes, len, i) {
                return Some(res);
            }
        }

        // 单独以 E01 开头
        if is_start && bytes[i] == b'E' && i + 1 < len && bytes[i + 1].is_ascii_digit() {
            let mut j = i + 1;
            while j < len && bytes[j].is_ascii_digit() {
                j += 1;
            }
            let digits = &upper[i + 1..j];
            if !plausible_episode(digits) {
                if is_calendar_date(digits) {
                    reject_date_token(&upper[i..j]);
                }
            } else if let Ok(ep) = digits.parse::<u32>() {
                let (ep_to, _) = parse_episode_range(&upper[j..]);
                let ep_to = ep_to.filter(|to| plausible_episode(&to.to_string()));
                return Some((i, Some(1), Some(ep), ep_to));
            }
        }

        i += 1;
    }

    None
}

/// 解析 E01-E04 或 -04 这种区间
fn parse_episode_range(after: &str) -> (Option<u32>, usize) {
    let bytes = after.as_bytes();
    let mut i = 0;
    let mut has_sep = false;
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'-' || bytes[i] == b'~') {
        if bytes[i] == b'-' || bytes[i] == b'~' {
            has_sep = true;
        }
        i += 1;
    }
    if !has_sep || i >= bytes.len() {
        return (None, 0);
    }
    let mut k = i;
    if bytes[k] == b'E' || bytes[k] == b'e' {
        k += 1;
        if k < bytes.len() && (bytes[k] == b'P' || bytes[k] == b'p') {
            k += 1;
        }
    }
    let digit_start = k;
    while k < bytes.len() && bytes[k].is_ascii_digit() {
        k += 1;
    }
    if k > digit_start {
        if let Ok(to) = after[digit_start..k].parse::<u32>() {
            return (Some(to), k);
        }
    }
    (None, 0)
}

/// 解析中文季集如 "第 2 季 第 5 集"、"第二季 第01-04集"
pub(super) fn find_chinese_season_episode(
    s: &str,
) -> Option<(usize, Option<u32>, Option<u32>, Option<u32>)> {
    let mut pos = None;
    let mut season = None;
    let mut episode = None;
    let mut episode_to = None;

    // 遍历所有 "第" 出现的位置
    for (p, _) in s.match_indices('第') {
        let after = &s[p + '第'.len_utf8()..];

        // 检查是否是第x季
        if let Some(j_pos) = after.find('季') {
            if j_pos <= 10 {
                let season_str = after[..j_pos].trim();
                if let Some(s_num) = parse_cn_or_arabic_num(season_str) {
                    if pos.is_none() || p < pos.unwrap() {
                        pos = Some(p);
                    }
                    season = Some(s_num);
                }
            }
        }

        // 检查是否是第x集 / 话
        if let Some(end_pos) = after
            .find('集')
            .or_else(|| after.find('话'))
            .or_else(|| after.find('話'))
        {
            if end_pos <= 20 {
                let ep_str = after[..end_pos].trim();
                let (from, to) = parse_chinese_ep_range(ep_str);
                if from.is_some() {
                    if pos.is_none() || p < pos.unwrap() {
                        pos = Some(p);
                    }
                    episode = from;
                    episode_to = to;
                }
            }
        }
    }

    if episode.is_some() && season.is_none() {
        season = Some(1);
    }

    pos.map(|p| (p, season, episode, episode_to))
}

fn parse_chinese_ep_range(s: &str) -> (Option<u32>, Option<u32>) {
    let parts: Vec<&str> = s
        .split(|c: char| c == '-' || c == '~' || c == '—')
        .collect();
    match parts.as_slice() {
        [single] => (parse_cn_or_arabic_num(single.trim()), None),
        [from, to] => {
            let f = parse_cn_or_arabic_num(from.trim());
            let t = parse_cn_or_arabic_num(to.trim());
            (
                f,
                t.filter(|to_val| f.is_some_and(|f_val| *to_val >= f_val)),
            )
        }
        _ => (None, None),
    }
}

fn parse_cn_or_arabic_num(s: &str) -> Option<u32> {
    if let Ok(n) = s.parse::<u32>() {
        return Some(n);
    }
    match s {
        "一" | "1" => Some(1),
        "二" | "两" | "2" => Some(2),
        "三" | "3" => Some(3),
        "四" | "4" => Some(4),
        "五" | "5" => Some(5),
        "六" | "6" => Some(6),
        "七" | "7" => Some(7),
        "八" | "8" => Some(8),
        "九" | "9" => Some(9),
        "十" | "10" => Some(10),
        _ => None,
    }
}
