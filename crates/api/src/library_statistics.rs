//! Library 统计：一次归属聚合与可见刷新状态（API-03）。
//!
//! 旧实现每个 Library 都重跑一遍全局账本读取、逐行 `get_media`，并对每个文件
//! `stat` 一次。这里改成：
//!
//! 1. 锁内**一次** ownership/role/ledger/media 批量聚合，得到每库计数；
//! 2. 体积取自浏览投影 facts 的**一次 SQL 聚合**（零 JSON 解码、零文件 IO）；
//! 3. `refreshed_at_ms` / `stale` 取自投影 head：可见刷新状态；
//! 4. 从未建过投影的库走一次**有界构建**，冷统计因此不返回假 0；scope 变化或
//!    facts 过旧只标脏并返回 `stale`，不在名册轮询里重扫文件系统。
//!
//! 计数始终来自台账（当前 scope），所以任何一条路径都不会把有内容的库报成 0；
//! 体积只在 facts 属于**当前 scope** 时才给出，旧 scope 的体积一律不复用。

use std::collections::{HashMap, HashSet};
use std::path::Path;

use domain::{MediaId, UserId};
use store::{BrowseFileTotals, Library, LibraryOwnership, ProjectionHead, Store};

use store::library_scope_signature as scope_signature;
use crate::browse_projection::facts::FACTS_VERSION;
use crate::blocking_budget::BlockingError;
use crate::http::library::selection::{BROWSE_DEADLINE, browse_work_error};
use crate::http::err;
use axum::http::StatusCode;
use crate::management::ApiState;

/// 一个 Library 的可见统计。
///
/// 前四个字段与既有 `stats` 同名同义；`refreshed_at_ms` / `stale` 是新增的兼容
/// 字段：前者是投影最后一次真实发布时间，后者表示体积不是当前 scope 的最新事实。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LibraryStatistics {
    pub item_count: u64,
    pub file_count: u64,
    pub total_size_bytes: u64,
    pub strm_file_count: u64,
    pub refreshed_at_ms: i64,
    pub stale: bool,
}

/// 一个 Library 的归属计数（锁内 DB 事实）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct OwnerCounts {
    pub item_count: u64,
    pub file_count: u64,
    pub strm_file_count: u64,
}

/// 一次聚合的结果：全部 Library 的计数（可见性由调用方按用户过滤）。
struct Aggregate {
    libraries: Vec<Library>,
    counts: Vec<OwnerCounts>,
}

impl Aggregate {
    fn libraries(&self) -> &[Library] {
        &self.libraries
    }

    fn counts_at(&self, slot: usize) -> OwnerCounts {
        self.counts[slot]
    }
}

/// 一个 Library 的锁内事实：计数 + 投影 head + 当前 scope 指纹是否一致。
struct LibraryPlan {
    library: Library,
    counts: OwnerCounts,
    head: Option<ProjectionHead>,
    scope_matches: bool,
}

/// facts 的可用性：决定体积从哪来、要不要构建或标脏。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FactsState {
    /// head ready、scope 一致，facts 与台账行数一致。
    Current,
    /// 同 scope 的 facts 可用，但比台账旧（还有未消费的变更）。
    Behind,
    /// 没有可用的 facts：从未建过投影 / scope 变了 / facts 版本过旧。
    Unusable { never_built: bool },
}

/// `.strm` 是几十字节的 URL 指针文本，不计入物理体积（与旧统计口径一致）。
fn is_strm(path: &str) -> bool {
    path.to_lowercase().ends_with(".strm")
}

/// 统计可见性：管理范围（admin）能看到全部 Library（含 removal 中与
/// `admin_visible=false` 的），成员/匿名只看「接受工作且对自己可见」的库。
///
/// 这是 `list_libraries` 过滤的**超集**：调用方仍按自己的 `scope`/`kind` 过滤条目，
/// 因此管理范围里那些不满足 `library_visible` 的库也能拿到真实计数。
fn stats_visible(
    ownership: &LibraryOwnership,
    library: &Library,
    user: Option<UserId>,
    is_admin: bool,
) -> bool {
    if is_admin {
        return true;
    }
    if !ownership.accepts_work(&library.id) {
        return false;
    }
    match user {
        None => true,
        Some(user_id) => {
            library.access_mode == "everyone" || library.member_ids.contains(&user_id)
        }
    }
}

/// 一次聚合：ownership 快照 + 全账本一次读取 + Media 批量读取。
///
/// 同一 Media 在多库各有拷贝时逐库计入（不做全局去重），没有 Media 行的台账行
/// 与并列根下的行都不计入任何库——与旧 `rows_in_library` 的口径逐字一致。
fn aggregate_counts(
    store: &Store,
    ownership: &LibraryOwnership,
) -> Result<Aggregate, store::StoreError> {
    let libraries: Vec<Library> = ownership.libraries().to_vec();
    let slots: HashMap<&str, usize> = libraries
        .iter()
        .enumerate()
        .map(|(slot, library)| (library.id.as_str(), slot))
        .collect();
    let mut counts = vec![OwnerCounts::default(); libraries.len()];
    let mut seen: Vec<HashSet<MediaId>> = vec![HashSet::new(); libraries.len()];
    let rows = store.list_ledger()?;
    let ids: Vec<MediaId> = rows
        .iter()
        .map(|row| row.media_id)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let media = store.media_batch(&ids)?;
    for row in &rows {
        let Some(media) = media.get(&row.media_id) else {
            continue;
        };
        let Some(owner) = ownership.owner(Path::new(&row.path), media.kind) else {
            continue;
        };
        let Some(&slot) = slots.get(owner.id.as_str()) else {
            continue;
        };
        counts[slot].file_count += 1;
        if is_strm(&row.path) {
            counts[slot].strm_file_count += 1;
        }
        if seen[slot].insert(row.media_id) {
            counts[slot].item_count += 1;
        }
    }
    Ok(Aggregate { libraries, counts })
}

fn is_admin_role(store: &Store, user: Option<UserId>) -> bool {
    user.map(|user_id| {
        store
            .user_role(user_id)
            .as_deref()
            .map(|role| role == "admin")
            .unwrap_or(false)
    })
    .unwrap_or(false)
}

/// 锁内一次收集：计数 + 每库 head + scope 指纹比对。role 只解析一次。
fn collect_plans(store: &Store, user: Option<UserId>) -> Result<Vec<LibraryPlan>, String> {
    let ownership = store.library_ownership().map_err(|error| error.to_string())?;
    let is_admin = is_admin_role(store, user);
    let aggregate = aggregate_counts(store, &ownership).map_err(|error| error.to_string())?;
    let mut plans = Vec::new();
    for (slot, library) in aggregate.libraries().iter().enumerate() {
        if !stats_visible(&ownership, library, user, is_admin) {
            continue;
        }
        let head = store
            .browse_projection_head(&library.id)
            .map_err(|error| error.to_string())?;
        let scope_matches = head
            .as_ref()
            .is_some_and(|head| head.scope_signature == scope_signature(library));
        plans.push(LibraryPlan {
            library: library.clone(),
            counts: aggregate.counts_at(slot),
            head,
            scope_matches,
        });
    }
    Ok(plans)
}

fn facts_state(plan: &LibraryPlan, totals: Option<&BrowseFileTotals>) -> FactsState {
    let ready = plan.head.as_ref().is_some_and(|head| head.ready);
    if !ready || !plan.scope_matches {
        return FactsState::Unusable {
            never_built: plan.head.is_none(),
        };
    }
    match totals {
        // 同 scope 但投影落后：体积仍是这个 scope 的旧事实，标 stale 并等重建。
        Some(totals) if totals.file_count != plan.counts.file_count => FactsState::Behind,
        Some(_) => FactsState::Current,
        None => FactsState::Unusable { never_built: false },
    }
}

fn statistics(plan: &LibraryPlan, totals: Option<&BrowseFileTotals>) -> LibraryStatistics {
    let refreshed_at_ms = plan.head.as_ref().map(|head| head.refreshed_at_ms).unwrap_or(0);
    let base = LibraryStatistics {
        item_count: plan.counts.item_count,
        file_count: plan.counts.file_count,
        strm_file_count: plan.counts.strm_file_count,
        total_size_bytes: 0,
        refreshed_at_ms,
        stale: true,
    };
    match facts_state(plan, totals) {
        FactsState::Current => LibraryStatistics {
            total_size_bytes: totals.map(|totals| totals.total_size_bytes).unwrap_or(0),
            stale: false,
            ..base
        },
        FactsState::Behind => LibraryStatistics {
            total_size_bytes: totals.map(|totals| totals.total_size_bytes).unwrap_or(0),
            ..base
        },
        FactsState::Unusable { .. } => base,
    }
}

/// 同步版本（配置写路径在锁内复用同一套口径）：不构建、不标脏，只读现有事实。
pub(crate) fn statistics_for(store: &Store, library: &Library) -> LibraryStatistics {
    let counts = store
        .library_ownership()
        .ok()
        .and_then(|ownership| aggregate_counts(store, &ownership).ok())
        .and_then(|aggregate| {
            aggregate
                .libraries()
                .iter()
                .position(|entry| entry.id == library.id)
                .map(|slot| aggregate.counts_at(slot))
        })
        .unwrap_or_default();
    let head = store.browse_projection_head(&library.id).ok().flatten();
    let scope_matches = head
        .as_ref()
        .is_some_and(|head| head.scope_signature == scope_signature(library));
    let totals = if head.as_ref().is_some_and(|head| head.ready) && scope_matches {
        store
            .browse_file_totals(&library.id, FACTS_VERSION)
            .ok()
            .flatten()
    } else {
        None
    };
    statistics(
        &LibraryPlan {
            library: library.clone(),
            counts,
            head,
            scope_matches,
        },
        totals.as_ref(),
    )
}

/// 统计读取的失败：HTTP 调用方要区分预算失败（503/504）与 Store 失败（500）。
pub(crate) enum StatisticsFailure {
    Budget(BlockingError),
    Store(String),
}

impl StatisticsFailure {
    pub(crate) fn into_response(self) -> axum::response::Response {
        match self {
            Self::Budget(error) => browse_work_error(error),
            Self::Store(message) => err(StatusCode::INTERNAL_SERVER_ERROR, "store.error", &message),
        }
    }

    fn into_message(self) -> String {
        match self {
            Self::Budget(error) => error.to_string(),
            Self::Store(message) => message,
        }
    }
}

/// 预算内一次读取这些 Library 的 facts 汇总（`browse_files` 的 SQL 聚合）。
///
/// 版本不符的行按「投影过旧」处理（返回 `None`），不把 0 当成空库。
async fn read_totals(
    state: &ApiState,
    ids: Vec<String>,
) -> Result<HashMap<String, BrowseFileTotals>, StatisticsFailure> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let state_clone = state.clone();
    state
        .http_work
        .run(BROWSE_DEADLINE, move || {
            let store = state_clone.store.lock();
            let mut totals = HashMap::with_capacity(ids.len());
            for id in &ids {
                match store.browse_file_totals(id, FACTS_VERSION) {
                    Ok(Some(entry)) => {
                        totals.insert(id.clone(), entry);
                    }
                    Ok(None) => {}
                    Err(error) => return Err(error.to_string()),
                }
            }
            Ok(totals)
        })
        .await
        .map_err(StatisticsFailure::Budget)?
        .map_err(StatisticsFailure::Store)
}

/// 需要 facts 汇总的库：head ready 且 scope 指纹与当前配置一致。
fn ready_ids(plans: &[LibraryPlan]) -> Vec<String> {
    plans
        .iter()
        .filter(|plan| plan.head.as_ref().is_some_and(|head| head.ready) && plan.scope_matches)
        .map(|plan| plan.library.id.clone())
        .collect()
}

/// 读取所有可见 Library 的统计。
pub(crate) async fn collect_statistics(
    state: ApiState,
    user: Option<UserId>,
) -> Result<HashMap<String, LibraryStatistics>, StatisticsFailure> {
    // 1) 锁内一次聚合：ownership/role/ledger/media batch → 每库计数 + head。
    let mut plans = {
        let store = state.store.lock();
        collect_plans(&store, user).map_err(StatisticsFailure::Store)?
    };
    // 2) 预算内：已就绪投影的 facts 一次 SQL 汇总。
    let mut totals = read_totals(&state, ready_ids(&plans)).await?;
    // 3) 从未建过投影的库：一次有界构建（单飞 + 共享容量），再按新代读 facts。
    let cold: Vec<usize> = plans
        .iter()
        .enumerate()
        .filter(|(_, plan)| {
            facts_state(plan, totals.get(&plan.library.id))
                == (FactsState::Unusable { never_built: true })
        })
        .map(|(slot, _)| slot)
        .collect();
    let mut built: Vec<String> = Vec::with_capacity(cold.len());
    for slot in cold {
        let library_id = plans[slot].library.id.clone();
        match state.projection.ensure(state.clone(), library_id.clone()).await {
            Ok(_) => built.push(library_id),
            Err(error) => {
                // 计数仍来自台账，只是没有可用的体积事实。
                tracing::debug!(library_id = %library_id, ?error, "Library 统计无法构建投影，先返回 stale 统计");
            }
        }
    }
    if !built.is_empty() {
        // 构建可能并发发布另一代：head 必须回读，不能拿返回的快照当当前代。
        let store = state.store.lock();
        for plan in plans
            .iter_mut()
            .filter(|plan| built.contains(&plan.library.id))
        {
            plan.head = store
                .browse_projection_head(&plan.library.id)
                .map_err(|error| StatisticsFailure::Store(error.to_string()))?;
            plan.scope_matches = plan
                .head
                .as_ref()
                .is_some_and(|head| head.scope_signature == scope_signature(&plan.library));
        }
        drop(store);
    }
    totals.extend(read_totals(&state, built).await?);
    // 4) 投影过旧（scope 变 / facts 版本旧 / 落后）：标脏交给投影刷新路径重建，
    //    本请求不重扫文件系统。
    let mut out = HashMap::with_capacity(plans.len());
    for plan in &plans {
        let totals = totals.get(&plan.library.id);
        match facts_state(plan, totals) {
            FactsState::Current => {}
            FactsState::Behind | FactsState::Unusable { never_built: false } => {
                state.projection.mark_dirty(&plan.library.id);
                tracing::debug!(
                    library_id = %plan.library.id,
                    "Library 统计发现投影过旧，已标脏等待重建"
                );
            }
            FactsState::Unusable { never_built: true } => {}
        }
        out.insert(plan.library.id.clone(), statistics(plan, totals));
    }
    Ok(out)
}

/// 一次读取所有可见 Library 的统计（HTTP 失败细节由调用方决定）。
pub async fn read_library_statistics(
    state: ApiState,
    user: Option<UserId>,
) -> Result<HashMap<String, LibraryStatistics>, String> {
    collect_statistics(state, user)
        .await
        .map_err(StatisticsFailure::into_message)
}
