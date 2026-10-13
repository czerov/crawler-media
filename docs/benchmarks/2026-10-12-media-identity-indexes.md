# DB6 Media 身份索引：历史测量与适用范围

**历史结果，2026-10-12；不是当前生产库或本次重新测量。** 实现提交 `11708be`；bundled SQLite3.50.2／rusqlite0.37；50000行合成Media，同进程30次取median。

| 查询 | 无索引 | 六索引后 |
|---|---|---|
| tmdb_id等值 | 4.06ms SCAN | 0.011ms SEARCH |
| title/kind/year(NULL) | 1.52ms SCAN | 0.014ms SEARCH |
| title或五alias的OR | 6.72ms SCAN | 0.035ms MULTI-INDEX OR |

结论：只建tmdb索引不能使所有OR分支可索引；采用五alias单列＋title/kind/year的非唯一索引。tmdb(kind)复合在该fixture没有额外收益。Movie/TV同数字alias合法，不加全局UNIQUE。

生产Store::open路径历史核验：alias0.012ms、title0.013ms、OR0.035ms，真实DDL创建并被使用。公开回归与测量入口见 [media_lookup_indexes](<../../crates/store/tests/media_lookup_indexes.rs>)；DDL见 [tables](<../../crates/store/src/schema/tables.rs>)。

**未测项：** 六索引对写入吞吐、磁盘大小、首次建索引启动耗时的完整量化；不能说成本“可忽略”。原证据中的live18行只是当时样本，已移除实时口吻。倍率不能外推当前生产SLA；[总复核](<../audits/2026-10-13-database-api-review.md>)与[后续测量门](<../superpowers/plans/2026-10-13-database-api-followup.md>)负责当前状态。
