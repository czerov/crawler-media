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
"#;
