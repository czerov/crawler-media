//! DB8 Task 4：数据库维护目标（allowlist）与**有界** SQLite 执行器。
//!
//! 设计约束（方案第 3.2 / 5.2 节）：
//!
//! - 路径只由内部事实产生：Store 真实 `app_path` + `data_dir` 下的固定文件名，
//!   外加**已存在**的可选 fanart 库。不接受任意用户路径，也不会创建空数据库。
//! - 独立连接 `READ_WRITE`（不带 CREATE），短 `busy_timeout=250ms`，不为维护重复
//!   迁移/seed，不执行 `wal_checkpoint(TRUNCATE)`，不删除/搬移 DB/WAL/SHM 文件。
//! - 轻量路径只有 `PRAGMA optimize`；VACUUM 需要显式开启 + 冷却 + 空闲页阈值 +
//!   磁盘准入，全部通过才执行。
//! - 每库有界执行：`InterruptHandle` + 看门狗线程，超时 `interrupt()` 并把该库标成
//!   `Failed`；正常完成时通过 channel 通知看门狗提前退出并 `join`，不留残留线程。

use std::path::{Path, PathBuf};
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use rusqlite::{Connection, OpenFlags, ffi};

use super::policy::{
    VACUUM_MIN_FREE_BYTES, VACUUM_MIN_FREE_RATIO_PERCENT, VACUUM_MIN_INTERVAL_SECS,
};
use super::report::{DatabaseOutcome, DatabaseReport};

/// 单库执行上限。超时走 SQLite interrupt，不用 Tokio timeout 假装已停止。
pub const PER_DATABASE_LIMIT: Duration = Duration::from_secs(30);

/// VACUUM 前的额外磁盘预留（方案第 5.2 节的 64MiB）。
pub const VACUUM_DISK_HEADROOM_BYTES: u64 = 64 * 1024 * 1024;

/// 维护连接的 `busy_timeout`：不沿用前台 5s，避免维护积压。
const MAINTENANCE_BUSY_TIMEOUT_MS: &str = "250";

/// 超时后重试中断的节奏。
///
/// 实测（rusqlite 0.37 + bundled SQLite）：
///
/// - `sqlite3_interrupt` 落在两条语句之间会被**丢弃**（SQLite 在一步结束时、
///   没有活跃 VDBE 时清标志），单次中断可能完全无效；
/// - VACUUM 的页拷贝阶段走 `sqlite3_backup`，**不检查** interrupt 标志。
///
/// 因此到期后以很短的节奏反复中断，直到主线程通知完成：落在可中断语句（`PRAGMA
/// optimize`、VACUUM 的拷贝前阶段）上的中断会让该库以 `SQLITE_INTERRUPT` 结束；
/// 若整段超时都落在页拷贝里，SQLite 无法停止，只能如实记录 `Vacuumed` 与超限的
/// `elapsed_ms`（见 DB8 报告"偏离"一节）。
const INTERRUPT_RETRY: Duration = Duration::from_millis(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DatabaseName {
    App,
    Catalog,
    Library,
    Subscribe,
    Jobs,
    Fanart,
}

impl DatabaseName {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::App => "app",
            Self::Catalog => "catalog",
            Self::Library => "library",
            Self::Subscribe => "subscribe",
            Self::Jobs => "jobs",
            Self::Fanart => "fanart",
        }
    }
}

#[derive(Clone, Debug)]
pub struct DatabaseTarget {
    pub name: DatabaseName,
    pub path: PathBuf,
}

/// 可选库：文件不存在时记 Skipped，**不**创建。
const OPTIONAL: [DatabaseName; 1] = [DatabaseName::Fanart];

fn data_dir_path(name: DatabaseName, data_dir: &Path) -> PathBuf {
    data_dir.join(format!("{}.db", name.as_str()))
}

/// 维护目标 allowlist：App 用 Store 的真实 `app_path`，其余是 `data_dir` 下的固定
/// 文件名；可选 fanart 仅当文件已存在才进入列表。
pub fn database_targets(data_dir: &Path, app_path: &Path) -> Vec<DatabaseTarget> {
    let mut targets = vec![
        DatabaseTarget {
            name: DatabaseName::App,
            path: app_path.to_path_buf(),
        },
        DatabaseTarget {
            name: DatabaseName::Catalog,
            path: data_dir_path(DatabaseName::Catalog, data_dir),
        },
        DatabaseTarget {
            name: DatabaseName::Library,
            path: data_dir_path(DatabaseName::Library, data_dir),
        },
        DatabaseTarget {
            name: DatabaseName::Subscribe,
            path: data_dir_path(DatabaseName::Subscribe, data_dir),
        },
        DatabaseTarget {
            name: DatabaseName::Jobs,
            path: data_dir_path(DatabaseName::Jobs, data_dir),
        },
    ];
    for name in OPTIONAL {
        let path = data_dir_path(name, data_dir);
        if path.is_file() {
            targets.push(DatabaseTarget { name, path });
        }
    }
    targets
}

/// 声明过但当前不存在的可选库：报告里记 Skipped，绝不创建文件。
pub(crate) fn missing_optional_targets(data_dir: &Path) -> Vec<DatabaseTarget> {
    OPTIONAL
        .iter()
        .map(|name| DatabaseTarget {
            name: *name,
            path: data_dir_path(*name, data_dir),
        })
        .filter(|target| !target.path.is_file())
        .collect()
}

/// 一次执行的只读上下文。
pub struct MaintenanceContext<'a> {
    pub now: i64,
    pub last_vacuum_success: Option<i64>,
    pub cancelled: &'a dyn Fn() -> bool,
}

pub trait DatabaseExecutor: Send + Sync {
    fn execute(
        &self,
        target: &DatabaseTarget,
        vacuum_enabled: bool,
        context: &MaintenanceContext<'_>,
    ) -> Result<DatabaseReport, String>;
}

/// 真实 SQLite 执行器的阈值与超时。生产用 policy 常量；测试可缩小/替换阈值，
/// 在小库上走通 VACUUM 与中断路径。
#[derive(Clone, Copy, Debug)]
pub struct SqliteLimits {
    pub min_free_bytes: u64,
    pub min_free_ratio_percent: u32,
    pub vacuum_headroom_bytes: u64,
    pub per_database_limit: Duration,
}

impl SqliteLimits {
    pub fn production() -> Self {
        Self {
            min_free_bytes: VACUUM_MIN_FREE_BYTES,
            min_free_ratio_percent: VACUUM_MIN_FREE_RATIO_PERCENT,
            vacuum_headroom_bytes: VACUUM_DISK_HEADROOM_BYTES,
            per_database_limit: PER_DATABASE_LIMIT,
        }
    }
}

impl Default for SqliteLimits {
    fn default() -> Self {
        Self::production()
    }
}

/// 生产执行器：policy 常量阈值 + 每库 30s 上限。
pub struct SqliteExecutor;

/// 同一套真实逻辑，但使用注入的阈值/超时（测试与诊断用）。
pub struct ConfiguredSqliteExecutor {
    limits: SqliteLimits,
}

impl ConfiguredSqliteExecutor {
    pub fn with_limits(limits: SqliteLimits) -> Self {
        Self { limits }
    }
}

impl DatabaseExecutor for SqliteExecutor {
    fn execute(
        &self,
        target: &DatabaseTarget,
        vacuum_enabled: bool,
        context: &MaintenanceContext<'_>,
    ) -> Result<DatabaseReport, String> {
        run_sqlite(target, vacuum_enabled, context, SqliteLimits::production())
    }
}

impl DatabaseExecutor for ConfiguredSqliteExecutor {
    fn execute(
        &self,
        target: &DatabaseTarget,
        vacuum_enabled: bool,
        context: &MaintenanceContext<'_>,
    ) -> Result<DatabaseReport, String> {
        run_sqlite(target, vacuum_enabled, context, self.limits)
    }
}

/// 打开一个真实的库（**不带 CREATE**），在有界看门狗下执行维护并记录报告。
///
/// 打不开真实库是 `Err`，不静默忽略；缺失的可选库已由 [`database_targets`] 提前排除。
fn run_sqlite(
    target: &DatabaseTarget,
    vacuum_enabled: bool,
    context: &MaintenanceContext<'_>,
    limits: SqliteLimits,
) -> Result<DatabaseReport, String> {
    let started = Instant::now();
    let size_before = file_len(&target.path);
    let conn = Connection::open_with_flags(&target.path, OpenFlags::SQLITE_OPEN_READ_WRITE)
        .map_err(|error| format!("打开数据库 {} 失败: {error}", target.path.display()))?;
    // journal_mode=WAL 是持久属性，已由 Store/Queue 写进文件头；维护连接不重复设置，
    // 避免为维护引入额外写锁。外键策略与前台保持一致。
    conn.pragma_update(None, "busy_timeout", MAINTENANCE_BUSY_TIMEOUT_MS)
        .map_err(|error| {
            format!(
                "设置 {} 的 busy_timeout 失败: {error}",
                target.path.display()
            )
        })?;
    let _ = conn.pragma_update(None, "foreign_keys", "ON");

    let interrupt = conn.get_interrupt_handle();
    let (done_tx, done_rx) = channel::<()>();
    let limit = limits.per_database_limit;
    let watchdog = std::thread::spawn(move || {
        let mut wait = limit;
        loop {
            match done_rx.recv_timeout(wait) {
                Ok(()) | Err(RecvTimeoutError::Disconnected) => return,
                Err(RecvTimeoutError::Timeout) => {
                    interrupt.interrupt();
                    wait = INTERRUPT_RETRY;
                }
            }
        }
    });

    let work = execute_work(&conn, target, vacuum_enabled, context, limits, size_before);
    // 正常完成（或被中断返回）后通知看门狗提前退出并 join，避免残留线程在连接释放后
    // 仍持有 InterruptHandle。
    let _ = done_tx.send(());
    let _ = watchdog.join();

    Ok(DatabaseReport {
        name: target.name.as_str().to_string(),
        outcome: work.outcome,
        free_pages: work.free_pages,
        page_size: work.page_size,
        size_before,
        size_after: file_len(&target.path),
        skip_reason: work.skip_reason,
        elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    })
}

struct WorkOutcome {
    outcome: DatabaseOutcome,
    skip_reason: Option<String>,
    free_pages: i64,
    page_size: i64,
}

fn execute_work(
    conn: &Connection,
    target: &DatabaseTarget,
    vacuum_enabled: bool,
    context: &MaintenanceContext<'_>,
    limits: SqliteLimits,
    size_before: u64,
) -> WorkOutcome {
    if let Err(error) = conn.execute_batch("PRAGMA optimize;") {
        return classify(&error, "PRAGMA optimize");
    }
    let page_size = pragma_i64(conn, "page_size");
    let page_count = pragma_i64(conn, "page_count");
    let free_pages = pragma_i64(conn, "freelist_count");
    let light = WorkOutcome {
        outcome: DatabaseOutcome::Optimized,
        skip_reason: None,
        free_pages,
        page_size,
    };
    if !vacuum_enabled {
        return light;
    }
    if (context.cancelled)() {
        return skip(
            "已执行 PRAGMA optimize；VACUUM 跳过：任务已取消",
            free_pages,
            page_size,
        );
    }
    let facts = VacuumFacts {
        free_pages: as_u64(free_pages),
        page_size: as_u64(page_size),
        page_count: as_u64(page_count),
        now: context.now,
        last_vacuum_success: context.last_vacuum_success,
        limits,
    };
    if let Some(reason) = vacuum_skip_reason(&facts) {
        return skip(reason, free_pages, page_size);
    }
    if let Some(reason) = disk_skip_reason(target, size_before, limits) {
        return skip(reason, free_pages, page_size);
    }
    match conn.execute_batch("VACUUM;") {
        Ok(()) => WorkOutcome {
            outcome: DatabaseOutcome::Vacuumed,
            skip_reason: None,
            free_pages,
            page_size,
        },
        Err(error) => classify(&error, "VACUUM"),
    }
}

/// VACUUM 准入事实（全部整数，避免溢出/除零/浮点误差）。
struct VacuumFacts {
    free_pages: u64,
    page_size: u64,
    page_count: u64,
    now: i64,
    last_vacuum_success: Option<i64>,
    limits: SqliteLimits,
}

/// 冷却与空闲页阈值。`Some(reason)` 表示必须跳过并写明哪一条不满足。
fn vacuum_skip_reason(facts: &VacuumFacts) -> Option<String> {
    if let Some(last) = facts.last_vacuum_success {
        let elapsed = u64::try_from(facts.now.saturating_sub(last)).unwrap_or(0);
        if elapsed < VACUUM_MIN_INTERVAL_SECS {
            return Some(format!(
                "冷却期未到（距上次成功 VACUUM {elapsed} 秒，需要 {VACUUM_MIN_INTERVAL_SECS} 秒）"
            ));
        }
    }
    let free_bytes = facts.free_pages.saturating_mul(facts.page_size);
    if free_bytes < facts.limits.min_free_bytes {
        return Some(format!(
            "空闲页 {free_bytes} 字节 < 阈值 {} 字节",
            facts.limits.min_free_bytes
        ));
    }
    let ratio = if facts.page_count == 0 {
        0
    } else {
        facts.free_pages.saturating_mul(100) / facts.page_count
    };
    if ratio < u64::from(facts.limits.min_free_ratio_percent) {
        return Some(format!(
            "空闲页比例 {ratio}% < 阈值 {}%",
            facts.limits.min_free_ratio_percent
        ));
    }
    None
}

/// 磁盘准入：保守准入，不承诺执行中空间不变；拿不到可用空间时也跳过。
fn disk_skip_reason(
    target: &DatabaseTarget,
    size_before: u64,
    limits: SqliteLimits,
) -> Option<String> {
    let required = size_before
        .saturating_mul(2)
        .saturating_add(limits.vacuum_headroom_bytes);
    match available_disk_bytes(&target.path) {
        Some(available) if available >= required => None,
        Some(available) => Some(format!(
            "已执行 PRAGMA optimize；VACUUM 跳过：磁盘可用空间 {available} 字节 < 需要 {required} 字节（2 × 主体 + 预留）"
        )),
        None => Some("已执行 PRAGMA optimize；VACUUM 跳过：无法确定可用磁盘空间".to_string()),
    }
}

fn skip(reason: impl Into<String>, free_pages: i64, page_size: i64) -> WorkOutcome {
    WorkOutcome {
        outcome: DatabaseOutcome::Skipped,
        skip_reason: Some(reason.into()),
        free_pages,
        page_size,
    }
}

/// busy/locked → 跳过（下轮再试）；其余（IO/corrupt/full/中断）→ 失败并保留上下文。
fn classify(error: &rusqlite::Error, step: &str) -> WorkOutcome {
    let text = format!("{step} 失败: {error}");
    let outcome = match error {
        rusqlite::Error::SqliteFailure(inner, _) => match inner.code {
            ffi::ErrorCode::DatabaseBusy | ffi::ErrorCode::DatabaseLocked => {
                return WorkOutcome {
                    outcome: DatabaseOutcome::Skipped,
                    skip_reason: Some(format!("{text}（数据库忙/被锁，下轮再试）")),
                    free_pages: 0,
                    page_size: 0,
                };
            }
            ffi::ErrorCode::OperationInterrupted => {
                return WorkOutcome {
                    outcome: DatabaseOutcome::Failed,
                    skip_reason: Some(format!("{text}（超过单库执行上限，已被中断）")),
                    free_pages: 0,
                    page_size: 0,
                };
            }
            _ => DatabaseOutcome::Failed,
        },
        _ => DatabaseOutcome::Failed,
    };
    WorkOutcome {
        outcome,
        skip_reason: Some(text),
        free_pages: 0,
        page_size: 0,
    }
}

fn as_u64(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

fn pragma_i64(conn: &Connection, name: &str) -> i64 {
    // 诊断读取：读不到按 0 处理，会让阈值判断保守地跳过 VACUUM。
    conn.pragma_query_value(None, name, |row| row.get(0))
        .unwrap_or(0)
}

fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path)
        .map(|metadata| metadata.len())
        .unwrap_or(0)
}

#[cfg(unix)]
fn available_disk_bytes(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;

    let raw = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: statvfs 只写它自己的 out 参数；raw 是合法的 NUL 结尾 C 字符串。
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    if unsafe { libc::statvfs(raw.as_ptr(), stats.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: 返回 0 表示 statvfs 已初始化 stats。
    let stats = unsafe { stats.assume_init() };
    let block = if stats.f_frsize > 0 {
        stats.f_frsize
    } else {
        stats.f_bsize
    };
    Some((stats.f_bavail as u64).saturating_mul(block as u64))
}

#[cfg(not(unix))]
fn available_disk_bytes(_path: &Path) -> Option<u64> {
    None
}
