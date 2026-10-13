//! 批量读取每个 def 的 live/last child，避免列表页的 N+1 查询。
//!
//! 选取规则与 `schedule::get_live_child` / `get_last_child` 完全一致：
//! live = queued 优先于 running，同状态 `run_after DESC`；
//! last = 终态中 `finished_at DESC, run_after DESC`。

use std::collections::HashMap;

use domain::JobDefId;
use rusqlite::Connection;

use crate::rows::{JOB_COLS, map_job};
use crate::types::{Job, JobError};

const DEF_CHUNK: usize = 500;

pub struct JobChildren {
    pub def_id: JobDefId,
    pub live: Option<Job>,
    pub last: Option<Job>,
}

/// 批量取每个 def 的 live/last child；输入顺序即输出顺序，重复 id 只保留一份。
pub fn children_for_defs(
    conn: &Connection,
    ids: &[JobDefId],
) -> Result<Vec<JobChildren>, JobError> {
    let mut order: Vec<JobDefId> = Vec::with_capacity(ids.len());
    let mut index: HashMap<JobDefId, usize> = HashMap::with_capacity(ids.len());
    for id in ids {
        if !index.contains_key(id) {
            index.insert(*id, order.len());
            order.push(*id);
        }
    }
    let mut children: Vec<JobChildren> = order
        .iter()
        .map(|def_id| JobChildren {
            def_id: *def_id,
            live: None,
            last: None,
        })
        .collect();

    for chunk in order.chunks(DEF_CHUNK) {
        collect_live(conn, chunk, &index, &mut children)?;
        collect_last(conn, chunk, &index, &mut children)?;
    }
    Ok(children)
}

fn collect_live(
    conn: &Connection,
    chunk: &[JobDefId],
    index: &HashMap<JobDefId, usize>,
    children: &mut [JobChildren],
) -> Result<(), JobError> {
    let placeholders = vec!["?"; chunk.len()].join(",");
    let sql = format!(
        "WITH ranked AS (
             SELECT {JOB_COLS},
                    ROW_NUMBER() OVER (
                        PARTITION BY def_id
                        ORDER BY CASE status WHEN 'queued' THEN 0 ELSE 1 END, run_after DESC
                    ) as rn
             FROM jobs
             WHERE def_id IN ({placeholders}) AND status IN ('queued','running')
         )
         SELECT {JOB_COLS} FROM ranked WHERE rn = 1"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(
        rusqlite::params_from_iter(chunk.iter().map(JobDefId::to_string)),
        map_job,
    )?;
    for row in rows {
        let job = row?;
        let Some(def_id) = job.def_id else { continue };
        let Some(slot) = index.get(&def_id) else {
            continue;
        };
        children[*slot].live = Some(job);
    }
    Ok(())
}

fn collect_last(
    conn: &Connection,
    chunk: &[JobDefId],
    index: &HashMap<JobDefId, usize>,
    children: &mut [JobChildren],
) -> Result<(), JobError> {
    let placeholders = vec!["?"; chunk.len()].join(",");
    // 使用 SQLite 窗口函数 ROW_NUMBER() 仅取出每个 def 的 top 1 最新终态记录，
    // 避免全量历史加载和反序列化，防止历史行损坏或大历史导致的内存/CPU开销。
    let sql = format!(
        "WITH ranked AS (
             SELECT {JOB_COLS},
                    ROW_NUMBER() OVER (
                        PARTITION BY def_id
                        ORDER BY finished_at DESC, run_after DESC
                    ) as rn
             FROM jobs
             WHERE def_id IN ({placeholders}) AND status IN ('succeeded','failed','cancelled')
         )
         SELECT {JOB_COLS} FROM ranked WHERE rn = 1"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(
        rusqlite::params_from_iter(chunk.iter().map(JobDefId::to_string)),
        map_job,
    )?;
    for row in rows {
        let job = row?;
        let Some(def_id) = job.def_id else { continue };
        let Some(slot) = index.get(&def_id) else {
            continue;
        };
        children[*slot].last = Some(job);
    }
    Ok(())
}
