//! 维护任务的执行报告（DB8 第 6 节）。
//!
//! 报告是**只读观测**：每轮执行的汇总 JSON 写入 `job.progress`，每库成功 VACUUM 的
//! 时间另存 `maintenance.vacuum_last_success.{database}` setting，都不覆盖用户策略。
//! 不记录 token、完整 payload 或播放明细。

/// 单库维护结果。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DatabaseReport {
    pub name: String,
    pub outcome: DatabaseOutcome,
    pub free_pages: i64,
    pub page_size: i64,
    pub size_before: u64,
    pub size_after: u64,
    pub skip_reason: Option<String>,
    pub elapsed_ms: u64,
}

impl DatabaseReport {
    /// 没有触碰数据库就跳过的条目（例如可选库文件不存在）。
    pub(crate) fn skipped(name: &str, reason: impl Into<String>) -> Self {
        Self {
            name: name.to_string(),
            outcome: DatabaseOutcome::Skipped,
            free_pages: 0,
            page_size: 0,
            size_before: 0,
            size_after: 0,
            skip_reason: Some(reason.into()),
            elapsed_ms: 0,
        }
    }

    /// 执行器直接失败（打不开库等）时的条目，错误上下文保留在 `skip_reason`。
    pub(crate) fn failed(name: &str, reason: impl Into<String>) -> Self {
        Self {
            name: name.to_string(),
            outcome: DatabaseOutcome::Failed,
            free_pages: 0,
            page_size: 0,
            size_before: 0,
            size_after: 0,
            skip_reason: Some(reason.into()),
            elapsed_ms: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DatabaseOutcome {
    Optimized,
    Vacuumed,
    Skipped,
    Failed,
}

/// 一轮维护执行的汇总报告（写入 `job.progress`）。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MaintenanceReport {
    pub kind: String,
    pub policy_version: u32,
    /// 保留类任务的固定窗口下界；数据库维护没有保留窗口，固定为 `None`。
    pub cutoff: Option<i64>,
    pub dry_run: bool,
    pub scanned: usize,
    pub candidates: usize,
    pub deleted: usize,
    pub protected: usize,
    pub batches: usize,
    pub budget_exhausted: bool,
    pub elapsed_ms: u64,
    /// 仅数据库维护非空：每库报告。
    pub databases: Vec<DatabaseReport>,
    /// 跳过原因（Library 工作运行中、任务取消等）。
    pub note: Option<String>,
}

impl MaintenanceReport {
    pub(crate) fn new(kind: jobs::JobKind, policy_version: u32) -> Self {
        Self {
            kind: kind.as_str().to_string(),
            policy_version,
            cutoff: None,
            dry_run: false,
            scanned: 0,
            candidates: 0,
            deleted: 0,
            protected: 0,
            batches: 0,
            budget_exhausted: false,
            elapsed_ms: 0,
            databases: Vec::new(),
            note: None,
        }
    }
}
