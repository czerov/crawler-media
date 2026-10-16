//! 三个源库各自的 durable browse-change journal（`_browse_changes`）schema。
//!
//! D3-R2-06：原先每个受影响行都无条件地在同一事务里追加一条事件（例如一次 marker
//! 刷新删除/重插 N 条剧集 marker，或 `delete_ledger_for_media` 在一个事务里删掉整部
//! 剧的台账），单事务内的事件产量与写放大没有上界，只有消费端裁剪是分块的。这里对
//! **产量**本身设上限：每个源 journal 最多保留 [`JOURNAL_EVENT_THRESHOLD`] 条逐行
//! 事件；达到阈值后，其余变更被合并为一条保守脏事件（`entity_kind = browse_all`、
//! `entity_key = *`），由 `NOT EXISTS` 保证只出现一次。
//!
//! 不丢失效、不删未确认事件：保守事件在被消费前一直留在 journal 中（消费端只在
//! `apply_browse_changes` 确认游标之后删除 `seq <= through`），被抑制的行事件都由它
//! 代表。消费端分支：`api::browse_projection::changes::ChangeConsumer::resolve_affected`
//! 对三个源的未知 `entity_kind` 走 `_ =>` 兜底分支，把 `ownership.libraries()` 中每个
//! Library 全部标脏（比“只有已发布投影依赖的库”更保守），因此保守事件等价于
//! “该源的所有库都失效”，只会多算、不会漏算。

use rusqlite::Connection;

use crate::StoreError;

/// 单个源 journal 的逐行事件上限（阈值）。
///
/// 取消费端单轮窗口 `MAX_EVENTS_PER_ROUND = 512` 的两倍：正常轮询节奏（每轮最多
/// 消费 512 条并裁剪已确认事件）下不会触发合并，而真正失控的单事务批量写会被压到
/// 阈值 + 1 条。计数口径是该源 journal 中**未确认**的事件总数（已确认事件被裁剪），
/// 所以阈值同时限制“跨多个小事务累积的 backlog”，方向保守（多标脏，不丢失效）。
pub const JOURNAL_EVENT_THRESHOLD: i64 = 1024;

/// 合并后的保守脏事件 kind。消费端把它当作未知 kind，走兜底分支全部标脏。
pub const COALESCED_EVENT_KIND: &str = "browse_all";

/// 合并后的保守脏事件 key；语义由 kind 决定，key 只是占位。
pub const COALESCED_EVENT_KEY: &str = "*";

/// `_browse_changes` 表与索引。三个源库各自一份，互不共享。
const JOURNAL_TABLE_DDL: &str = "CREATE TABLE IF NOT EXISTS _browse_changes (
        seq INTEGER PRIMARY KEY AUTOINCREMENT,
        entity_kind TEXT NOT NULL,
        entity_key TEXT NOT NULL,
        old_path TEXT,
        new_path TEXT
    );
    CREATE INDEX IF NOT EXISTS _browse_changes_seq_idx ON _browse_changes(seq);
    CREATE INDEX IF NOT EXISTS _browse_changes_coalesced_idx
        ON _browse_changes(entity_kind) WHERE entity_kind = 'browse_all';";

/// 一个被记入 journal 的源表事件。
struct JournalEvent<'a> {
    /// 触发器名；与旧 schema 同名，升级时被替换。
    trigger: &'a str,
    table: &'a str,
    /// `INSERT` / `UPDATE` / `DELETE`。
    operation: &'a str,
    /// journaled entity kind。
    kind: &'a str,
    /// 生成 entity_key 的 SQL 表达式，可引用 `NEW`/`OLD`。
    key: &'a str,
    old_path: &'a str,
    new_path: &'a str,
}

impl JournalEvent<'_> {
    /// 逐行事件 + 一次性保守事件的触发器 DDL。
    ///
    /// 先 `DROP TRIGGER IF EXISTS` 再 `CREATE TRIGGER`：`migrate_*` 每次打开库都执行
    /// （不受 `user_version` 门控），老库里的无上限触发器必须被替换掉。
    ///
    /// 两条 `INSERT ... SELECT ... WHERE` 在同一触发器体内按顺序求值：
    /// 1. 逐行事件仅在“还没有保守事件、且未确认事件数未达阈值”时追加；
    /// 2. 保守事件仅在“未确认事件数已达阈值、且还不存在保守事件”时追加。
    /// 因此单事务内产量上界是 `JOURNAL_EVENT_THRESHOLD` 条逐行事件 + 1 条保守事件。
    fn ddl(&self) -> String {
        let JournalEvent {
            trigger,
            table,
            operation,
            kind,
            key,
            old_path,
            new_path,
        } = self;
        format!(
            "DROP TRIGGER IF EXISTS {trigger};
             CREATE TRIGGER {trigger} AFTER {operation} ON {table}
             BEGIN
                 INSERT INTO _browse_changes (entity_kind, entity_key, old_path, new_path)
                 SELECT '{kind}', {key}, {old_path}, {new_path}
                 WHERE NOT EXISTS (SELECT 1 FROM _browse_changes WHERE entity_kind = '{COALESCED_EVENT_KIND}')
                   AND (SELECT COUNT(*) FROM _browse_changes) < {JOURNAL_EVENT_THRESHOLD};
                 INSERT INTO _browse_changes (entity_kind, entity_key, old_path, new_path)
                 SELECT '{COALESCED_EVENT_KIND}', '{COALESCED_EVENT_KEY}', NULL, NULL
                 WHERE (SELECT COUNT(*) FROM _browse_changes) >= {JOURNAL_EVENT_THRESHOLD}
                   AND NOT EXISTS (SELECT 1 FROM _browse_changes WHERE entity_kind = '{COALESCED_EVENT_KIND}');
             END;"
        )
    }
}

fn journal_ddl(events: &[JournalEvent<'_>]) -> String {
    events
        .iter()
        .map(JournalEvent::ddl)
        .collect::<Vec<_>>()
        .join("\n")
}

const APP_EVENTS: [JournalEvent<'static>; 9] = [
    JournalEvent {
        trigger: "trg_app_media_changes_ins",
        table: "media",
        operation: "INSERT",
        kind: "media",
        key: "NEW.id",
        old_path: "NULL",
        new_path: "NULL",
    },
    JournalEvent {
        trigger: "trg_app_media_changes_upd",
        table: "media",
        operation: "UPDATE",
        kind: "media",
        key: "NEW.id",
        old_path: "NULL",
        new_path: "NULL",
    },
    JournalEvent {
        trigger: "trg_app_media_changes_del",
        table: "media",
        operation: "DELETE",
        kind: "media",
        key: "OLD.id",
        old_path: "NULL",
        new_path: "NULL",
    },
    JournalEvent {
        trigger: "trg_app_libraries_changes_ins",
        table: "libraries",
        operation: "INSERT",
        kind: "library",
        key: "NEW.id",
        old_path: "NULL",
        new_path: "NULL",
    },
    JournalEvent {
        trigger: "trg_app_libraries_changes_upd",
        table: "libraries",
        operation: "UPDATE",
        kind: "library",
        key: "NEW.id",
        old_path: "NULL",
        new_path: "NULL",
    },
    JournalEvent {
        trigger: "trg_app_libraries_changes_del",
        table: "libraries",
        operation: "DELETE",
        kind: "library",
        key: "OLD.id",
        old_path: "NULL",
        new_path: "NULL",
    },
    JournalEvent {
        trigger: "trg_app_roots_changes_ins",
        table: "library_roots",
        operation: "INSERT",
        kind: "library_root",
        key: "NEW.id",
        old_path: "NULL",
        new_path: "NEW.path",
    },
    JournalEvent {
        trigger: "trg_app_roots_changes_upd",
        table: "library_roots",
        operation: "UPDATE",
        kind: "library_root",
        key: "NEW.id",
        old_path: "OLD.path",
        new_path: "NEW.path",
    },
    JournalEvent {
        trigger: "trg_app_roots_changes_del",
        table: "library_roots",
        operation: "DELETE",
        kind: "library_root",
        key: "OLD.id",
        old_path: "OLD.path",
        new_path: "NULL",
    },
];

const CATALOG_EVENTS: [JournalEvent<'static>; 3] = [
    JournalEvent {
        trigger: "trg_cat_cache_changes_ins",
        table: "catalog_cache",
        operation: "INSERT",
        kind: "catalog_cache",
        key: "NEW.source || ':' || NEW.cache_key",
        old_path: "NULL",
        new_path: "NULL",
    },
    JournalEvent {
        trigger: "trg_cat_cache_changes_upd",
        table: "catalog_cache",
        operation: "UPDATE",
        kind: "catalog_cache",
        key: "NEW.source || ':' || NEW.cache_key",
        old_path: "NULL",
        new_path: "NULL",
    },
    JournalEvent {
        trigger: "trg_cat_cache_changes_del",
        table: "catalog_cache",
        operation: "DELETE",
        kind: "catalog_cache",
        key: "OLD.source || ':' || OLD.cache_key",
        old_path: "NULL",
        new_path: "NULL",
    },
];

const LIBRARY_EVENTS: [JournalEvent<'static>; 9] = [
    JournalEvent {
        trigger: "trg_lib_ledger_changes_ins",
        table: "ledger",
        operation: "INSERT",
        kind: "ledger",
        key: "NEW.id",
        old_path: "NULL",
        new_path: "NEW.path",
    },
    JournalEvent {
        trigger: "trg_lib_ledger_changes_upd",
        table: "ledger",
        operation: "UPDATE",
        kind: "ledger",
        key: "NEW.id",
        old_path: "OLD.path",
        new_path: "NEW.path",
    },
    JournalEvent {
        trigger: "trg_lib_ledger_changes_del",
        table: "ledger",
        operation: "DELETE",
        kind: "ledger",
        key: "OLD.id",
        old_path: "OLD.path",
        new_path: "NULL",
    },
    JournalEvent {
        trigger: "trg_lib_file_meta_changes_ins",
        table: "file_meta",
        operation: "INSERT",
        kind: "file_meta",
        key: "NEW.ledger_id",
        old_path: "NULL",
        new_path: "NULL",
    },
    JournalEvent {
        trigger: "trg_lib_file_meta_changes_upd",
        table: "file_meta",
        operation: "UPDATE",
        kind: "file_meta",
        key: "NEW.ledger_id",
        old_path: "NULL",
        new_path: "NULL",
    },
    JournalEvent {
        trigger: "trg_lib_file_meta_changes_del",
        table: "file_meta",
        operation: "DELETE",
        kind: "file_meta",
        key: "OLD.ledger_id",
        old_path: "NULL",
        new_path: "NULL",
    },
    JournalEvent {
        trigger: "trg_lib_markers_changes_ins",
        table: "media_markers",
        operation: "INSERT",
        kind: "media_marker",
        key: "'marker:' || NEW.media_id || ':' || NEW.season || ':' || NEW.episode",
        old_path: "NULL",
        new_path: "NULL",
    },
    JournalEvent {
        trigger: "trg_lib_markers_changes_upd",
        table: "media_markers",
        operation: "UPDATE",
        kind: "media_marker",
        key: "'marker:' || NEW.media_id || ':' || NEW.season || ':' || NEW.episode",
        old_path: "NULL",
        new_path: "NULL",
    },
    JournalEvent {
        trigger: "trg_lib_markers_changes_del",
        table: "media_markers",
        operation: "DELETE",
        kind: "media_marker",
        key: "'marker:' || OLD.media_id || ':' || OLD.season || ':' || OLD.episode",
        old_path: "NULL",
        new_path: "NULL",
    },
];

pub fn migrate_browse_changes_app(conn: &Connection) -> Result<(), StoreError> {
    conn.execute_batch(&format!(
        "{JOURNAL_TABLE_DDL}\n{}",
        journal_ddl(&APP_EVENTS)
    ))?;
    Ok(())
}

pub fn migrate_browse_changes_catalog(conn: &Connection) -> Result<(), StoreError> {
    conn.execute_batch(&format!(
        "{JOURNAL_TABLE_DDL}\n{}",
        journal_ddl(&CATALOG_EVENTS)
    ))?;
    Ok(())
}

pub fn migrate_browse_changes_library(conn: &Connection) -> Result<(), StoreError> {
    conn.execute_batch(&format!(
        "{JOURNAL_TABLE_DDL}
         CREATE TABLE IF NOT EXISTS browse_change_cursors (
             source TEXT PRIMARY KEY,
             last_seq INTEGER NOT NULL DEFAULT 0
         );
         {}",
        journal_ddl(&LIBRARY_EVENTS)
    ))?;
    Ok(())
}
