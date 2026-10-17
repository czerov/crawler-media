//! 浏览热页：只读投影 head → 选页 → 只读本页 facts。
//!
//! 常用排序（无 watch、无过滤）先用 `browse_media` 的索引列 SQL 选页；高级查询
//! 先用整库轻候选**完整**筛选排序（选页后过滤会漏项），两者都只在选页后解码本页
//! Media 的 media/file facts。任何非新鲜投影、事实不可解析或预算失败一律交回既有
//! 全量路径（`warn!` 留痕），因此输出与旧路径逐字段等价。
//!
//! OFFSET 深翻页仍有 SQLite 自身的跳过成本，本模块不承诺恒定代价。

use std::collections::{HashMap, HashSet};

use axum::http::StatusCode;
use axum::response::Response;
use domain::{LedgerRow, Media, MediaId, UserId};

use crate::browse_projection::facts::{FileFacts, MediaFacts};
use crate::http::err;
use crate::http::library::page::PageWindow;
use crate::management::ApiState;
use crate::store::{
    BrowseMediaCandidate, BrowseMediaFacts, BrowsePageOrder, BrowsePageRequest, ProjectionHead,
    Store, UnitState,
};

use super::metadata::BrowseMetadata;
use super::projected::{self, ArtworkFacts, ProjectedFacts};
use super::snapshot::{load_browse_snapshot_or_projection, select_snapshot};
use super::{
    BROWSE_DEADLINE, BrowseArtwork, IndexPageOrder, LibraryFilter, LibrarySelection, LightFacts,
    browse_work_error, units_for_media, watch_state_from_keys, watch_state_from_units,
};

/// items 热页：过滤排序后取本页。
pub(crate) async fn load_browse_page(
    state: &ApiState,
    library: &crate::store::Library,
    user: Option<UserId>,
    query: &HashMap<String, String>,
) -> Result<(Vec<LibrarySelection>, ProjectedFacts), Response> {
    load_page(state, library, user, query, PageKind::Items).await
}

/// gallery 热页：先按图片存在性剔除空组，再取本页（与旧 `gallery_page_items` 同口径）。
pub(crate) async fn load_browse_gallery_page(
    state: &ApiState,
    library: &crate::store::Library,
    user: Option<UserId>,
    query: &HashMap<String, String>,
) -> Result<(Vec<LibrarySelection>, ProjectedFacts), Response> {
    load_page(state, library, user, query, PageKind::Gallery).await
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PageKind {
    Items,
    Gallery,
}

async fn load_page(
    state: &ApiState,
    library: &crate::store::Library,
    user: Option<UserId>,
    query: &HashMap<String, String>,
    kind: PageKind,
) -> Result<(Vec<LibrarySelection>, ProjectedFacts), Response> {
    let filter = LibraryFilter::parse(query).map_err(|message| invalid(&message))?;
    let page = PageWindow::parse(query).map_err(|message| invalid(&message))?;
    // 省略 limit 仍是「返回全部」：直接走全量路径，行为不变。
    if page.limit.is_some() {
        // 只读 head：不装载整份投影行。批准的那一代在下面按页读取的**同一个**锁内复核。
        match state.projection.fresh_head(state, &library.id).await {
            Ok(Some(approved)) => {
                // 测试 seam：批准之后、按页读取之前插入并发变更（生产 no-op）。
                state.projection.run_page_approval_barrier(state, &library.id);
                let state_clone = state.clone();
                let library_id = library.id.clone();
                let work = state
                    .http_work
                    .run(BROWSE_DEADLINE, move || {
                        let store = state_clone.store.lock();
                        if !page_head_is_current(&state_clone, &store, &library_id, &approved) {
                            return Err(
                                "投影代在批准后发生变化（标脏/换代/scope）".to_string()
                            );
                        }
                        page_from_projection(
                            &state_clone,
                            &store,
                            &library_id,
                            user,
                            &filter,
                            page,
                            kind,
                        )
                    })
                    .await;
                match work {
                    Ok(Ok(loaded)) => return Ok(loaded),
                    Ok(Err(message)) => tracing::warn!(
                        library_id = %library.id,
                        %message,
                        "投影页事实不可用，回退到全量读取"
                    ),
                    Err(error) => return Err(browse_work_error(error)),
                }
            }
            Ok(None) => {}
            Err(error) => tracing::debug!(
                library_id = %library.id,
                ?error,
                "浏览投影 head 非新鲜，走重建/回退路径"
            ),
        }
    }
    full_page(state, library, user, query, page, kind).await
}

/// 在**同一个** Store 锁内复核被批准的那一代是否仍然可用：内存脏标记、DB
/// `ready`/scope、`generation` 与 `dirty_token` 都必须未变。
///
/// 批准（只读 head）与按页读取是两次加锁；中间若有别的请求消费了影响本库的变更
/// （把 head 标脏或换代），旧 facts 仍留在表里，这里必须拒绝并回退重建。
fn page_head_is_current(
    state: &ApiState,
    store: &Store,
    library_id: &str,
    approved: &ProjectionHead,
) -> bool {
    if state.projection.is_dirty(library_id) {
        return false;
    }
    store
        .browse_page_head_is_current(library_id, approved)
        .unwrap_or(false)
}

/// 投影页读取（blocking 预算内）：选页 → 只解码本页 facts。
fn page_from_projection(
    state: &ApiState,
    store: &Store,
    library_id: &str,
    user: Option<UserId>,
    filter: &LibraryFilter,
    page: PageWindow,
    kind: PageKind,
) -> Result<(Vec<LibrarySelection>, ProjectedFacts), String> {
    let Some(limit) = page.limit else {
        return Err("projection page path requires an explicit limit".into());
    };
    let selection = page_ids(state, store, library_id, user, filter, page, limit, kind)?;
    let loaded = hydrate_page(state, store, library_id, user, filter, selection)?;
    tracing::debug!(
        library_id,
        page_items = loaded.0.len(),
        sort = %filter.sort,
        "浏览页按选中 Media 读取本页 facts"
    );
    Ok(loaded)
}

/// 选页结果：本页 Media id 顺序；常用排序的 SQL 分页顺带取回了 media 行，
/// 水合直接复用，避免同一页再读一遍。
struct PageSelection {
    ids: Vec<MediaId>,
    media_rows: Option<Vec<BrowseMediaFacts>>,
}

/// 选页：常用排序走 SQL 索引；其余（过滤/watch/非常用排序/gallery 图片剔除）先取
/// 整库轻候选完整筛选排序。两条路径都只返回本页 Media id。
fn page_ids(
    state: &ApiState,
    store: &Store,
    library_id: &str,
    user: Option<UserId>,
    filter: &LibraryFilter,
    page: PageWindow,
    limit: usize,
    kind: PageKind,
) -> Result<PageSelection, String> {
    if kind == PageKind::Items
        && let Some(order) = filter.index_page_order()
    {
        let request = BrowsePageRequest {
            library_id: library_id.to_string(),
            offset: page.offset,
            limit,
            order: match order {
                IndexPageOrder::Title => BrowsePageOrder::Title,
                IndexPageOrder::AddedAt => BrowsePageOrder::AddedAt,
                IndexPageOrder::ReleaseDate => BrowsePageOrder::ReleaseDate,
            },
            descending: filter.descending(),
        };
        let rows = store
            .browse_media_page(&request)
            .map_err(|error| error.to_string())?;
        // 计数在读取处：这一页的 media facts 行确实被读出并解析。
        state.count_decoded(0, rows.len());
        return Ok(PageSelection {
            ids: rows.iter().map(|row| row.media_id).collect(),
            media_rows: Some(rows),
        });
    }
    let candidates = store
        .browse_media_candidates(library_id)
        .map_err(|error| error.to_string())?;
    state.count_candidate_rows(candidates.len());
    let units = candidate_units(store, user, filter, &candidates)?;
    let needs_watch = filter.needs_watch_state();
    let mut items: Vec<LibrarySelection> = candidates
        .into_iter()
        .filter(|candidate| kind != PageKind::Gallery || has_images(candidate))
        .map(|candidate| candidate_selection(candidate, &units, user, needs_watch))
        .collect();
    items.retain(|item| filter.matches(item));
    items.sort_by(|left, right| filter.compare(left, right));
    // 高级查询的残余成本是整库轻候选 + watch unit（O(M+U)），显式留痕：
    // 这条路径不是 seek，也不解码非本页的重 facts。
    tracing::debug!(
        library_id,
        candidates = items.len(),
        watch_units = units.len(),
        sort = %filter.sort,
        "浏览高级查询按轻候选完整筛选排序（残余 O(M+U)）"
    );
    let bounds = page.bounds(items.len());
    Ok(PageSelection {
        ids: items[bounds].iter().map(|item| item.media.id).collect(),
        media_rows: None,
    })
}

/// watch 相关查询才需要 unit：`favorite` 只在输出（本页水合）里用，选页前不读。
fn candidate_units(
    store: &Store,
    user: Option<UserId>,
    filter: &LibraryFilter,
    candidates: &[BrowseMediaCandidate],
) -> Result<HashMap<(MediaId, i32, i32), UnitState>, String> {
    let (Some(user), true) = (user, filter.needs_watch_state()) else {
        return Ok(HashMap::new());
    };
    let ids: Vec<MediaId> = candidates
        .iter()
        .map(|candidate| candidate.media_id)
        .collect();
    units_for_media(store, user, &ids)
}

/// 与 `gallery_images` 同一图片存在性口径：海报 / 背景 / 任一单元剧照。
fn has_images(candidate: &BrowseMediaCandidate) -> bool {
    candidate.has_poster || candidate.has_backdrop || candidate.has_still
}

/// 选页前的行：`rows` 为空，`media` 只填过滤/排序用到的字段（选页后由本页 media
/// facts 水合完整字段），轻事实与 watch 与全量路径同口径。
fn candidate_selection(
    candidate: BrowseMediaCandidate,
    units: &HashMap<(MediaId, i32, i32), UnitState>,
    user: Option<UserId>,
    needs_watch: bool,
) -> LibrarySelection {
    let mut item = LibrarySelection {
        media: Media {
            id: candidate.media_id,
            kind: candidate.kind,
            title: candidate.title.clone(),
            year: candidate.year,
            original_title: None,
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        },
        rows: Vec::new(),
        metadata: BrowseMetadata {
            genres: candidate.genres.clone(),
            countries: candidate.countries.clone(),
            rating: candidate.rating,
            runtime: candidate.runtime,
            language: candidate.language.clone(),
            release_date: (!candidate.release_date.is_empty())
                .then(|| candidate.release_date.clone()),
            scraped: candidate.scraped,
        },
        favorite: false,
        played: false,
        seen: false,
        missing: candidate.missing,
        added_at: candidate
            .added_at_ns
            .and_then(|ns| projected::ns_to_system_time(ns).ok()),
        size_bytes: candidate.size_bytes,
        last_played_at: None,
        light: LightFacts::from_candidate(&candidate),
    };
    if user.is_some() {
        // 与 `collect_items` 同口径：收藏永远来自 whole unit；详细观看状态只在
        // 请求真的需要时才算。
        let whole = units.get(&(item.media.id, crate::store::UNIT_WHOLE, crate::store::UNIT_WHOLE));
        item.favorite = whole.is_some_and(|unit| unit.favorite);
        if needs_watch {
            let state = watch_state_from_keys(&item.media, candidate.episode_keys.iter().copied(), units);
            item.favorite = state.favorite;
            item.played = state.played;
            item.seen = state.seen;
            item.last_played_at = state.last_played_at;
        }
    }
    item
}

/// 选页后水合：只解码本页 Media 的 media/file facts 与本页 unit。
fn hydrate_page(
    state: &ApiState,
    store: &Store,
    library_id: &str,
    user: Option<UserId>,
    filter: &LibraryFilter,
    selection: PageSelection,
) -> Result<(Vec<LibrarySelection>, ProjectedFacts), String> {
    let PageSelection { ids, media_rows } = selection;
    let media_rows = match media_rows {
        Some(rows) => rows,
        None => {
            let rows = store
                .browse_media_facts_for(library_id, &ids)
                .map_err(|error| error.to_string())?;
            // 计数在读取处：读到的行都算，即使随后因缺 file 行被跳过。
            state.count_decoded(0, rows.len());
            rows
        }
    };
    let file_rows = store
        .browse_file_facts_for_media(library_id, &ids)
        .map_err(|error| error.to_string())?;
    // 计数在读取处：包含随后被丢弃（无对应 media 行/Media 无 file 行）与解析失败的行。
    state.count_decoded(file_rows.len(), 0);
    let units = match user {
        Some(user) => units_for_media(store, user, &ids)?,
        None => HashMap::new(),
    };
    let mut media_by_id: HashMap<MediaId, BrowseMediaFacts> = media_rows
        .into_iter()
        .map(|row| (row.media_id, row))
        .collect();
    let mut files_by_media: HashMap<MediaId, Vec<FileFacts>> = HashMap::new();
    for entry in file_rows {
        let facts = FileFacts::from_json(&entry.facts)
            .ok_or_else(|| format!("file facts unreadable for {}", entry.ledger_id))?;
        files_by_media.entry(entry.media_id).or_default().push(facts);
    }

    let mut artwork = HashMap::new();
    let mut stills = HashSet::new();
    let mut items = Vec::with_capacity(ids.len());
    for media_id in ids {
        let Some(media_row) = media_by_id.remove(&media_id) else {
            continue;
        };
        let media_facts = MediaFacts::from_json(&media_row.facts)
            .ok_or_else(|| format!("media facts unreadable for {media_id}"))?;
        let facts = files_by_media.remove(&media_id).unwrap_or_default();
        if facts.is_empty() {
            // 与全量路径一致：没有 ledger 行的 Media 不出现在浏览结果里。
            continue;
        }
        let rows: Vec<LedgerRow> = facts.iter().map(|fact| fact.row.clone()).collect();
        for fact in &facts {
            if fact.has_still {
                stills.insert(fact.row.id);
            }
        }
        artwork.insert(
            media_id,
            ArtworkFacts {
                has_poster: media_facts.has_poster,
                has_backdrop: media_facts.has_backdrop,
            },
        );
        let mut item = LibrarySelection {
            light: LightFacts::from_rows(&media_facts.media, &media_facts.metadata, &rows),
            media: media_facts.media,
            metadata: media_facts.metadata,
            favorite: false,
            played: false,
            seen: false,
            missing: facts.iter().any(|fact| fact.missing),
            added_at: projected::min_added_at(&facts)?,
            size_bytes: projected::total_size(&facts),
            last_played_at: None,
            rows,
        };
        if user.is_some() {
            let whole = units.get(&(media_id, crate::store::UNIT_WHOLE, crate::store::UNIT_WHOLE));
            item.favorite = whole.is_some_and(|unit| unit.favorite);
            if filter.needs_watch_state() {
                let state = watch_state_from_units(&item.media, &item.rows, &units);
                item.favorite = state.favorite;
                item.played = state.played;
                item.seen = state.seen;
                item.last_played_at = state.last_played_at;
            }
        }
        items.push(item);
    }
    Ok((items, ProjectedFacts { artwork, stills }))
}

/// 回退：既有全量读取（投影未就绪/不可解析、或省略 limit）。
async fn full_page(
    state: &ApiState,
    library: &crate::store::Library,
    user: Option<UserId>,
    query: &HashMap<String, String>,
    page: PageWindow,
    kind: PageKind,
) -> Result<(Vec<LibrarySelection>, ProjectedFacts), Response> {
    let (snapshot, artwork) = load_browse_snapshot_or_projection(state, library, user).await?;
    let files = state.browse_files.clone();
    let query = query.clone();
    let work = state
        .http_work
        .run(BROWSE_DEADLINE, move || {
            let selected = select_snapshot(snapshot, &query, files.as_ref())?;
            let art = BrowseArtwork::new(artwork.as_ref(), files.as_ref());
            let indices: Vec<usize> = match kind {
                PageKind::Gallery => gallery_page_indices(&selected, page, &art),
                PageKind::Items => page.bounds(selected.len()).collect(),
            };
            Ok(take_window(selected, &indices, &art))
        })
        .await;
    match work {
        Ok(result) => result,
        Err(error) => Err(browse_work_error(error)),
    }
}

/// 先按图片存在性剔除空组，再取分页窗口（返回命中窗口的下标）。
///
/// 只扫描到填满 `offset + limit` 个非空组为止（省略 limit 时扫描全部），因此
/// 空组不会造成短页。
fn gallery_page_indices(
    selected: &[LibrarySelection],
    page: PageWindow,
    art: &BrowseArtwork<'_>,
) -> Vec<usize> {
    let end = match page.limit {
        Some(limit) => page.offset.saturating_add(limit),
        None => usize::MAX,
    };
    let mut out = Vec::new();
    let mut non_empty_seen = 0usize;
    for (index, item) in selected.iter().enumerate() {
        if !has_gallery_images(item, art) {
            continue;
        }
        if non_empty_seen >= page.offset {
            out.push(index);
        }
        non_empty_seen += 1;
        if non_empty_seen >= end {
            break;
        }
    }
    out
}

/// 与 [`gallery_images`](crate::http::library_gallery) 同一图片存在性口径。
fn has_gallery_images(item: &LibrarySelection, art: &BrowseArtwork<'_>) -> bool {
    let facts = art.media(item.media.id, &item.rows);
    facts.has_poster || facts.has_backdrop || item.rows.iter().any(|row| art.has_still(row))
}

/// 取窗口内的条目，并按同一份图片事实物化 [`ProjectedFacts`]。
///
/// 投影来源时 `art` 是零 IO 查表；旧全量来源时它按旧语义现场探测，本函数只对
/// 窗口内的条目探测——与旧实现「渲染时才探测」等价。
fn take_window(
    selected: Vec<LibrarySelection>,
    indices: &[usize],
    art: &BrowseArtwork<'_>,
) -> (Vec<LibrarySelection>, ProjectedFacts) {
    let mut wanted: HashSet<usize> = indices.iter().copied().collect();
    let mut artwork = HashMap::new();
    let mut stills = HashSet::new();
    let mut items = Vec::with_capacity(indices.len());
    for (index, item) in selected.into_iter().enumerate() {
        if !wanted.remove(&index) {
            continue;
        }
        artwork.insert(item.media.id, art.media(item.media.id, &item.rows));
        for row in &item.rows {
            if art.has_still(row) {
                stills.insert(row.id);
            }
        }
        items.push(item);
    }
    (items, ProjectedFacts { artwork, stills })
}

fn invalid(message: &str) -> Response {
    tracing::error!(%message, "Library browse query rejected");
    err(StatusCode::BAD_REQUEST, "library.invalid", message)
}
