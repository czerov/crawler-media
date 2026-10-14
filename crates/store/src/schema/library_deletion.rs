pub const TABLES: &str = r#"
CREATE TABLE IF NOT EXISTS library_removals (
    operation_id TEXT PRIMARY KEY,
    library_id TEXT NOT NULL UNIQUE,
    state TEXT NOT NULL,
    snapshot_json TEXT NOT NULL,
    removed_ledger_count INTEGER NOT NULL DEFAULT 0,
    error TEXT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL
);
-- 已删除 Library 的根目录边界。与 `library_removals` 同生命周期（含 succeeded），
-- 是 `path_in_removal_scope` 的唯一来源：路径检查只读这里的小表，不再反序列化
-- `snapshot_json`（那份快照可能列出被删库的每个文件）。
CREATE TABLE IF NOT EXISTS library_removal_roots (
    operation_id TEXT NOT NULL,
    path TEXT NOT NULL,
    PRIMARY KEY (operation_id, path)
);
"#;
