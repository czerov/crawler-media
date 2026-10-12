//! 显式分页窗口：`limit`/`offset` 的校验与溢出安全切片。
//!
//! **省略 `limit` 时返回全部**是既有行为，本模块不改变它——迁移默认分页需要先
//! 清点所有调用者（前端、CLI、首页、目录树），属独立决策。这里只把**显式**参数
//! 从"静默接受任意值"改成"越界报错"，并让切片对 `offset + limit` 溢出免疫。

use std::collections::HashMap;

/// 显式 `limit` 的上限。超过就报错，不静默 clamp。
pub(crate) const MAX_PAGE_LIMIT: usize = 200;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PageWindow {
    pub offset: usize,
    /// `None` = 未显式指定 `limit` → 返回全部（既有行为）。
    pub limit: Option<usize>,
}

impl PageWindow {
    pub(crate) fn parse(query: &HashMap<String, String>) -> Result<Self, String> {
        let offset = match query.get("offset") {
            None => 0,
            Some(raw) => raw
                .parse::<usize>()
                .map_err(|_| format!("offset 必须是非负整数: {raw}"))?,
        };
        let limit = match query.get("limit") {
            None => None,
            Some(raw) => {
                let value = raw
                    .parse::<usize>()
                    .map_err(|_| format!("limit 必须是正整数: {raw}"))?;
                if value == 0 || value > MAX_PAGE_LIMIT {
                    return Err(format!("limit 必须在 1..={MAX_PAGE_LIMIT} 之间: {raw}"));
                }
                Some(value)
            }
        };
        Ok(Self { offset, limit })
    }

    /// 溢出安全切片：`offset` 超过长度时得到空区间；`offset + limit` 不会回绕。
    pub(crate) fn bounds(&self, len: usize) -> std::ops::Range<usize> {
        let start = self.offset.min(len);
        let end = match self.limit {
            Some(limit) => start.saturating_add(limit).min(len),
            None => len,
        };
        start..end
    }
}

#[cfg(test)]
mod tests {
    use super::{PageWindow, MAX_PAGE_LIMIT};

    fn query(pairs: &[(&str, &str)]) -> std::collections::HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn omitted_limit_keeps_returning_everything() {
        let window = PageWindow::parse(&query(&[])).unwrap();
        assert_eq!(window.limit, None);
        assert_eq!(window.bounds(10), 0..10);
    }

    #[test]
    fn explicit_limit_is_validated_not_clamped() {
        for bad in ["0", "201", "-1", "abc", ""] {
            assert!(
                PageWindow::parse(&query(&[("limit", bad)])).is_err(),
                "limit={bad} 必须被拒绝"
            );
        }
        assert_eq!(
            PageWindow::parse(&query(&[("limit", "200")])).unwrap().limit,
            Some(MAX_PAGE_LIMIT)
        );
        assert_eq!(
            PageWindow::parse(&query(&[("limit", "1")])).unwrap().limit,
            Some(1)
        );
    }

    #[test]
    fn offset_is_validated_and_bounds_never_overflow() {
        for bad in ["-1", "abc", ""] {
            assert!(
                PageWindow::parse(&query(&[("offset", bad)])).is_err(),
                "offset={bad} 必须被拒绝"
            );
        }
        let window = PageWindow::parse(&query(&[
            ("offset", &usize::MAX.to_string()),
            ("limit", "10"),
        ]))
        .unwrap();
        assert_eq!(window.bounds(10), 10..10, "offset 超出长度应为空区间");
        let window = PageWindow::parse(&query(&[
            ("offset", &(usize::MAX - 5).to_string()),
            ("limit", "200"),
        ]))
        .unwrap();
        assert_eq!(window.bounds(10), 10..10);
    }

    #[test]
    fn bounds_windows_the_middle_of_a_list() {
        let window = PageWindow::parse(&query(&[("offset", "2"), ("limit", "3")])).unwrap();
        assert_eq!(window.bounds(10), 2..5);
        assert_eq!(window.bounds(3), 2..3);
        assert_eq!(window.bounds(1), 1..1);
    }
}
