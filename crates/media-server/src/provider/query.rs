//! Jellyfin 条目选择的共享语义：parent 分类、过滤、排序、计数与分页。
//!
//! 这里只放**纯**逻辑，以及基于 trait 的旧式（全量快照）选择：协议路由与
//! provider 的默认实现复用同一份语义，provider 不必依赖 routes 的私有模块，
//! 也不会出现两份规则。生产 provider 覆盖
//! [`MediaServerProvider::query_visible_items`]，用轻候选先选页、再只为该页构建
//! tracks/chapters/NFO 细节；默认实现按旧方法等价过滤，**不承诺有界**。

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use domain::{MediaKind, UserId};

use super::{MediaItemSnapshot, MediaServerProvider, ServerLibrary};

/// 协议层的 typed 条目查询。字段与路由的 `ItemsQuery` 一一对应；默认值
/// （`recursive` 缺省 true、`limit` 的 clamp、`offset`）由路由构造时决定。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ServerItemQuery {
    pub parent_id: Option<String>,
    pub series_id: Option<String>,
    pub season_id: Option<String>,
    pub person_ids: Option<String>,
    pub filters: Option<String>,
    pub is_favorite: Option<bool>,
    pub is_played: Option<bool>,
    pub sort_by: Option<String>,
    pub sort_order: Option<String>,
    pub include_item_types: Option<String>,
    pub recursive: bool,
    pub is_folder: Option<bool>,
    pub search_term: Option<String>,
    pub genres: Option<String>,
    pub genre_ids: Option<String>,
    pub studio_ids: Option<String>,
    pub offset: usize,
    pub limit: usize,
}

impl ServerItemQuery {
    /// 这次请求要不要「分集」条目：`IncludeItemTypes` 里有 Episode，或者请求本身
    /// 就锁定了 Series/Season（`SeriesId`/`SeasonId` 是列分集的请求，与
    /// `IncludeItemTypes` 无关）。
    pub fn wants_episodes(&self) -> bool {
        self.series_id.is_some()
            || self.season_id.is_some()
            || self.item_types_contain("Episode")
    }

    /// 这次请求要不要「剧」条目：`IncludeItemTypes` 里有 Series，或者没给类型
    /// （缺省列表本来就把剧折叠成一条）。
    pub fn wants_series(&self) -> bool {
        match self.include_item_types.as_deref() {
            Some(_) => self.item_types_contain("Series"),
            None => true,
        }
    }

    fn item_types_contain(&self, kind: &str) -> bool {
        self.include_item_types
            .as_deref()
            .map(|types| types.split(',').any(|item| item.trim().eq_ignore_ascii_case(kind)))
            .unwrap_or(false)
    }
}

/// 一页条目：`total` 是**完整筛选后**（分页前）的条数，即协议里的
/// `TotalRecordCount`。
#[derive(Clone, Debug, Default)]
pub struct ServerItemPage {
    pub items: Vec<MediaItemSnapshot>,
    pub total: usize,
}

impl ServerItemPage {
    pub fn from_rows(rows: Vec<MediaItemSnapshot>, offset: usize, limit: usize) -> Self {
        let total = rows.len();
        let items = rows.into_iter().skip(offset).take(limit).collect();
        Self { items, total }
    }
}

/// `ParentId` 分类。`Genre`/`Studio` 只能靠 item metadata 判断，由调用方用
/// **已经取到的** items 升级，避免为了判断 genre 再整库读一次。
pub enum ParentScope {
    Root,
    Library(String),
    Series(String),
    Folder(PathBuf),
    Genre(String),
    Studio(String),
    Unknown,
}

/// `folder_<uuid5(root)>`：协议层给库根分配的 folder id（唯一来源）。
pub fn folder_id_for_root(root: &Path) -> String {
    format!(
        "folder_{}",
        uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, root.to_string_lossy().as_bytes()).simple()
    )
}

/// `folder_<uuid5(root)>` 对应的库根目录。
pub fn folder_root(parent: &str, libraries: &[ServerLibrary]) -> Option<PathBuf> {
    libraries.iter().find_map(|library| {
        library
            .root_paths
            .iter()
            .find_map(|root| (folder_id_for_root(root) == parent).then(|| root.clone()))
    })
}

/// 该 parent 是否就是某个可见库的 id（廉价判断，不读 items）。
pub fn library_parent<'a>(parent: Option<&'a str>, libraries: &[ServerLibrary]) -> Option<&'a str> {
    let parent = parent?;
    libraries
        .iter()
        .any(|library| library.id == parent)
        .then_some(parent)
}

/// 分类 parent：只有完全不带 parent/series 才是 `Root`；认不出来是 `Unknown`。
///
/// 优先级与既有实现一致：library > series > folder。
pub async fn resolve_parent_scope(
    provider: &(impl MediaServerProvider + ?Sized),
    user: UserId,
    parent: Option<&str>,
    series: Option<&str>,
    libraries: &[ServerLibrary],
) -> Result<ParentScope, String> {
    let is_folder = parent
        .map(|value| value.starts_with("folder_"))
        .unwrap_or(false);
    let is_library = library_parent(parent, libraries).is_some();
    let is_series = !is_folder
        && (series.is_some()
            || (!is_library
                && match parent {
                    Some(id) => provider
                        .resolve_single_item(user, id)
                        .await?
                        .map(|item| item.is_series)
                        .unwrap_or(false),
                    None => false,
                }));
    if is_library {
        return Ok(ParentScope::Library(parent.unwrap_or_default().to_string()));
    }
    if is_series {
        // parent 存在时优先，与既有 `parent.or(series)` 一致。
        let id = parent.or(series).unwrap_or_default().to_string();
        return Ok(ParentScope::Series(id));
    }
    if is_folder {
        return Ok(folder_root(parent.unwrap_or_default(), libraries)
            .map(ParentScope::Folder)
            .unwrap_or(ParentScope::Unknown));
    }
    if parent.is_none() {
        return Ok(ParentScope::Root);
    }
    Ok(ParentScope::Unknown)
}

/// 用可见 items 判断 parent 是否指向某个 genre/studio（名字，或 uuid5 的连字符 /
/// 紧凑形式）。两者都不是则 `Unknown`——调用方必须返回空集合，不能退回整库。
pub fn classify_genre_or_studio(parent: &str, items: &[MediaItemSnapshot]) -> ParentScope {
    classify_genre_or_studio_facts(
        parent,
        items
            .iter()
            .map(|item| (item.metadata.genres.as_slice(), item.metadata.studios.as_slice())),
    )
}

/// 同上，但基于轻候选的 `(genres, studios)` 视图：生产 provider 不必为分类
/// 组装完整快照。
pub fn classify_genre_or_studio_facts<'a>(
    parent: &str,
    items: impl Iterator<Item = (&'a [String], &'a [String])>,
) -> ParentScope {
    let mut genre = false;
    let mut studio = false;
    for (genres, studios) in items {
        genre |= genres.iter().any(|value| matches_genre_or_studio(parent, value));
        studio |= studios.iter().any(|value| matches_genre_or_studio(parent, value));
    }
    if genre {
        ParentScope::Genre(parent.to_string())
    } else if studio {
        ParentScope::Studio(parent.to_string())
    } else {
        ParentScope::Unknown
    }
}

/// 单个 genre/studio 名字是否命中 target（名字或 uuid5 的两种拼写）。
pub fn matches_genre_or_studio(target: &str, value: &str) -> bool {
    if value.eq_ignore_ascii_case(target) {
        return true;
    }
    let hyphenated = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, value.as_bytes())
        .hyphenated()
        .to_string();
    let simple = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, value.as_bytes())
        .simple()
        .to_string();
    target.eq_ignore_ascii_case(&hyphenated) || target.eq_ignore_ascii_case(&simple)
}

/// 一个条目的 genres/studios 是否命中 target。
pub fn item_matches_genre_or_studio(target: &str, genres: &[String], studios: &[String]) -> bool {
    genres
        .iter()
        .any(|value| matches_genre_or_studio(target, value))
        || studios
            .iter()
            .any(|value| matches_genre_or_studio(target, value))
}

/// `SeasonId` 去掉 `SeriesId` 前缀后的季号。
pub fn parse_season_id(series_id: &str, season_id: &str) -> Option<u32> {
    let series_id = series_id.replace('-', "");
    let season_id = season_id.replace('-', "");
    let suffix = season_id.get(series_id.len()..)?;
    if !season_id[..series_id.len()].eq_ignore_ascii_case(&series_id) {
        return None;
    }
    suffix.parse().ok()
}

/// 过滤 + 排序 + 计数 + 取页（旧式全量快照语义）。
pub fn select_page(rows: Vec<MediaItemSnapshot>, query: &ServerItemQuery) -> ServerItemPage {
    let rows = filter_items(rows, query);
    let (default_sort, default_order) = default_sort_for(&rows);
    let rows = sort_items(
        rows,
        query.sort_by.as_deref().unwrap_or(default_sort),
        query.sort_order.as_deref().unwrap_or(default_order),
    );
    ServerItemPage::from_rows(rows, query.offset, query.limit)
}

/// 全是分集时用季/集号升序，否则入库时间倒序——与既有缺省排序一致。
pub fn default_sort_for(rows: &[MediaItemSnapshot]) -> (&'static str, &'static str) {
    default_sort_for_items(rows.iter())
}

/// 同上，但接受任意快照视图（生产 provider 排轻候选时不必组装 Vec）。
pub fn default_sort_for_items<'a>(
    items: impl Iterator<Item = &'a MediaItemSnapshot>,
) -> (&'static str, &'static str) {
    let mut any = false;
    let mut episodes_only = true;
    for item in items {
        any = true;
        episodes_only &= item.media.kind == MediaKind::Tv && !item.is_series;
    }
    if any && episodes_only {
        ("ParentIndexNumber,IndexNumber", "Ascending")
    } else {
        ("DateCreated,SortName", "Descending")
    }
}

pub fn filter_items(
    rows: Vec<MediaItemSnapshot>,
    query: &ServerItemQuery,
) -> Vec<MediaItemSnapshot> {
    filter_items_except_played(rows, query)
        .into_iter()
        .filter(|item| matches_bool(item.played, query.is_played))
        .collect()
}

/// 单条命中判定：全量路径与生产 provider 的轻候选共用同一份谓词，避免两份语义。
pub fn matches_query(item: &MediaItemSnapshot, query: &ServerItemQuery) -> bool {
    matches_filter_predicates(item, query) && matches_bool(item.played, query.is_played)
}

/// 除播放状态以外的全部过滤条件。`Items/Latest` 要自己决定播放状态那一档
/// （显式 `IsPlayed` 才筛，未指定时只把已看完的沉底，见 `library::played_last`）。
pub fn filter_items_except_played(
    rows: Vec<MediaItemSnapshot>,
    query: &ServerItemQuery,
) -> Vec<MediaItemSnapshot> {
    rows.into_iter()
        .filter(|item| matches_filter_predicates(item, query))
        .collect()
}

/// 除播放状态以外的谓词集合：`filter_items_except_played`、`matches_query` 与
/// 生产 provider 的轻候选共用这一份。
fn matches_filter_predicates(item: &MediaItemSnapshot, query: &ServerItemQuery) -> bool {
    let person_ids = query.person_ids.as_deref().map(parse_person_ids);
    matches_series(item, query.series_id.as_deref())
        && matches_season(item, query.season_id.as_deref())
        && matches_people(item, person_ids.as_ref())
        && matches_user_marks(item, query.filters.as_deref())
        && matches_bool(item.metadata.is_favorite, query.is_favorite)
        && matches_include_item_types(item, query.include_item_types.as_deref())
        && matches_is_folder(item, query.is_folder)
        && matches_search_term(item, query.search_term.as_deref())
        && matches_genres(item, query.genres.as_deref(), query.genre_ids.as_deref())
        && matches_studios(item, query.studio_ids.as_deref())
}

fn matches_is_folder(item: &MediaItemSnapshot, is_folder: Option<bool>) -> bool {
    let Some(expected) = is_folder else {
        return true;
    };
    item.is_series == expected
}

fn matches_search_term(item: &MediaItemSnapshot, search_term: Option<&str>) -> bool {
    let Some(term) = search_term.map(str::trim).filter(|s| !s.is_empty()) else {
        return true;
    };
    let term_lower = term.to_lowercase();
    if item.media.title.to_lowercase().contains(&term_lower) {
        return true;
    }
    if let Some(orig) = &item.metadata.original_title {
        if orig.to_lowercase().contains(&term_lower) {
            return true;
        }
    }
    if let Some(overview) = &item.metadata.overview {
        if overview.to_lowercase().contains(&term_lower) {
            return true;
        }
    }
    false
}

fn matches_genres(item: &MediaItemSnapshot, genres: Option<&str>, genre_ids: Option<&str>) -> bool {
    let mut target_genres: Vec<&str> = Vec::new();
    if let Some(g) = genres {
        target_genres.extend(
            g.split(|c| c == ',' || c == '|')
                .map(str::trim)
                .filter(|s| !s.is_empty()),
        );
    }
    if let Some(gid) = genre_ids {
        target_genres.extend(
            gid.split(|c| c == ',' || c == '|')
                .map(str::trim)
                .filter(|s| !s.is_empty()),
        );
    }
    if target_genres.is_empty() {
        return true;
    }
    target_genres.iter().any(|tg| {
        item.metadata.genres.iter().any(|g| {
            if g.eq_ignore_ascii_case(tg) {
                return true;
            }
            let simple_id = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, g.as_bytes())
                .simple()
                .to_string();
            let hyphen_id = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, g.as_bytes())
                .hyphenated()
                .to_string();
            let prefixed = format!("genre_{simple_id}");
            tg.eq_ignore_ascii_case(&simple_id)
                || tg.eq_ignore_ascii_case(&hyphen_id)
                || tg.eq_ignore_ascii_case(&prefixed)
        })
    })
}

fn matches_studios(item: &MediaItemSnapshot, studio_ids: Option<&str>) -> bool {
    let Some(studios) = studio_ids.map(str::trim).filter(|s| !s.is_empty()) else {
        return true;
    };
    let targets: Vec<&str> = studios
        .split(|c| c == ',' || c == '|')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if targets.is_empty() {
        return true;
    }
    targets
        .iter()
        .any(|ts| item.metadata.studios.iter().any(|s| s.eq_ignore_ascii_case(ts)))
}

pub fn sort_items(
    mut rows: Vec<MediaItemSnapshot>,
    sort_by: &str,
    sort_order: &str,
) -> Vec<MediaItemSnapshot> {
    sort_snapshots(&mut rows, |item| item, sort_by, sort_order);
    rows
}

/// 按同一份排序规则排序任意「快照视图」（生产 provider 直接排轻候选）。
pub fn sort_snapshots<T>(
    rows: &mut [T],
    snapshot: impl Fn(&T) -> &MediaItemSnapshot,
    sort_by: &str,
    sort_order: &str,
) {
    let keys: Vec<_> = sort_by
        .split(',')
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .collect();
    let sort_orders: Vec<_> = sort_order.split(',').map(str::trim).collect();
    rows.sort_by(|left, right| {
        let left = snapshot(left);
        let right = snapshot(right);
        keys.iter()
            .enumerate()
            .map(|(index, key)| {
                let ordering = match key.to_ascii_lowercase().as_str() {
                    "datecreated" => left.metadata.date_created.cmp(&right.metadata.date_created),
                    "sortname" | "name" => left
                        .media
                        .title
                        .to_lowercase()
                        .cmp(&right.media.title.to_lowercase()),
                    "parentindexnumber" => left.row.season.cmp(&right.row.season),
                    "indexnumber" => left.row.episode.cmp(&right.row.episode),
                    _ => std::cmp::Ordering::Equal,
                };
                let order = sort_orders
                    .get(index)
                    .or_else(|| sort_orders.last())
                    .copied()
                    .unwrap_or("Descending");
                if order.eq_ignore_ascii_case("descending") {
                    ordering.reverse()
                } else {
                    ordering
                }
            })
            .find(|ordering| *ordering != std::cmp::Ordering::Equal)
            .unwrap_or_else(|| left.media.id.to_string().cmp(&right.media.id.to_string()))
    });
}

fn matches_include_item_types(item: &MediaItemSnapshot, include_item_types: Option<&str>) -> bool {
    let Some(types) = include_item_types else {
        return true;
    };
    let actual = if item.is_series {
        "Series"
    } else {
        match item.media.kind {
            MediaKind::Movie => "Movie",
            MediaKind::Tv => "Episode",
            MediaKind::Video => "Video",
        }
    };
    types
        .split(',')
        .map(str::trim)
        .any(|kind| kind.eq_ignore_ascii_case(actual))
}

fn matches_bool(actual: bool, expected: Option<bool>) -> bool {
    expected.map_or(true, |expected| actual == expected)
}

fn matches_user_marks(item: &MediaItemSnapshot, filters: Option<&str>) -> bool {
    let Some(filters) = filters else {
        return true;
    };
    filters
        .split(',')
        .map(str::trim)
        .filter(|filter| !filter.is_empty())
        .all(|filter| match filter.to_ascii_lowercase().as_str() {
            "isfavorite" => item.metadata.is_favorite,
            "isplayed" => item.played,
            "isunplayed" => !item.played,
            _ => true,
        })
}

fn parse_person_ids(person_ids: &str) -> HashSet<String> {
    person_ids
        .split(',')
        .map(str::trim)
        .map(compact_id)
        .collect()
}

fn matches_series(item: &MediaItemSnapshot, series_id: Option<&str>) -> bool {
    let Some(series_id) = series_id else {
        return true;
    };
    !item.is_series
        && item.media.kind == MediaKind::Tv
        && compact_id(&item.media.id.to_string()) == compact_id(series_id)
}

fn matches_season(item: &MediaItemSnapshot, season_id: Option<&str>) -> bool {
    let Some(season_id) = season_id else {
        return true;
    };
    let series_id = compact_id(&item.media.id.to_string());
    parse_season_id(&series_id, season_id)
        .is_some_and(|season| !item.is_series && item.row.season == Some(season))
}

fn matches_people(item: &MediaItemSnapshot, person_ids: Option<&HashSet<String>>) -> bool {
    let Some(person_ids) = person_ids else {
        return true;
    };
    item.metadata.people.iter().any(|person| {
        person_ids.contains(&compact_id(&person.id))
            || person
                .tmdb_id
                .as_ref()
                .is_some_and(|tmdb_id| person_ids.contains(tmdb_id))
    })
}

fn compact_id(id: &str) -> String {
    id.replace('-', "").to_ascii_lowercase()
}

/// 默认实现（未迁移 provider）的条目选择：按旧方法取快照，再做纯过滤/排序/分页。
///
/// 分集展开仍按「每个 series 再查一次」的方式做，所以**不承诺有界**；生产
/// provider 必须覆盖 [`MediaServerProvider::query_visible_items`]。这里已经修正
/// 的三件事与生产路径一致：folder 归属在折叠 series 之前确定、`SeriesId`/
/// `SeasonId` 本身就是列分集的请求、任何读取失败都向上抛（绝不退化成空页）。
pub async fn legacy_select(
    provider: &(impl MediaServerProvider + ?Sized),
    user: UserId,
    query: ServerItemQuery,
) -> Result<ServerItemPage, String> {
    let libraries = provider.list_visible_libraries(user).await?;
    let parent = query.parent_id.as_deref();
    let series = query.series_id.as_deref();
    let scope = resolve_parent_scope(provider, user, parent, series, &libraries).await?;
    let is_folder_id = parent
        .map(|value| value.starts_with("folder_"))
        .unwrap_or(false);
    if is_folder_id && !matches!(scope, ParentScope::Folder(_)) {
        // 认不出的 folder 既不是可见库根、也不是 genre/studio：空集合，绝不整库。
        return Ok(ServerItemPage::default());
    }
    let needs_items = matches!(scope, ParentScope::Unknown);
    let parent_id_for_provider = match &scope {
        ParentScope::Library(id) | ParentScope::Series(id) => Some(id.as_str()),
        _ => None,
    };
    let mut rows = provider.list_visible_items(user, parent_id_for_provider).await?;
    let wants_episodes = query.wants_episodes();
    let wants_series = query.wants_series();
    // folder 归属必须先于折叠：代表行在别的根下不能决定整个 series 是否属于
    // 这个 folder。先取未折叠的分集，再用 folder 内的行重建剧快照。
    if let ParentScope::Folder(root) = &scope {
        if rows.iter().any(|row| row.is_series) {
            let mut expanded = Vec::new();
            for item in rows {
                if item.is_series {
                    let series_id = item.media.id.to_string().replace('-', "");
                    let episodes = provider.list_visible_items(user, Some(&series_id)).await?;
                    let in_folder: Vec<MediaItemSnapshot> = episodes
                        .into_iter()
                        .filter(|episode| Path::new(&episode.row.path).starts_with(root))
                        .collect();
                    if wants_series {
                        if let Some(representative) = in_folder.first() {
                            let mut series = item;
                            series.row = representative.row.clone();
                            series.metadata.child_count = Some(in_folder.len());
                            series.metadata.season_count = Some(
                                in_folder.iter().filter_map(|episode| episode.row.season).collect::<HashSet<_>>().len(),
                            );
                            series.unplayed_item_count = Some(in_folder.iter().filter(|episode| !episode.played).count());
                            series.played = in_folder.iter().all(|episode| episode.played);
                            expanded.push(series);
                        }
                    }
                    if wants_episodes {
                        expanded.extend(in_folder);
                    }
                } else if Path::new(&item.row.path).starts_with(root)
                    && (item.media.kind != MediaKind::Tv
                        || item.is_series && wants_series
                        || !item.is_series && wants_episodes)
                {
                    expanded.push(item);
                }
            }
            rows = expanded;
        } else {
            rows.retain(|item| {
                Path::new(&item.row.path).starts_with(root)
                    && (item.is_series && wants_series || !item.is_series && wants_episodes)
            });
        }
    } else if wants_episodes && rows.iter().any(|row| row.is_series) {
        let mut expanded = Vec::new();
        for item in rows {
            if item.is_series {
                let series_id = item.media.id.to_string().replace('-', "");
                let episodes = match &scope {
                    ParentScope::Library(library_id) => {
                        provider.list_visible_items_in_library(user, Some(&series_id), library_id).await?
                    }
                    _ => provider.list_visible_items(user, Some(&series_id)).await?,
                };
                if wants_episodes {
                    expanded.extend(episodes);
                }
                if wants_series {
                    expanded.push(item);
                }
            } else if item.is_series && wants_series || !item.is_series && wants_episodes {
                expanded.push(item);
            }
        }
        rows = expanded;
    }
    let genre_target = if needs_items {
        match classify_genre_or_studio(parent.unwrap_or_default(), &rows) {
            ParentScope::Genre(id) | ParentScope::Studio(id) => Some(id),
            _ => None,
        }
    } else {
        None
    };
    if let Some(target_id) = genre_target {
        rows.retain(|item| {
            item_matches_genre_or_studio(
                &target_id,
                item.metadata.genres.as_slice(),
                item.metadata.studios.as_slice(),
            )
        });
    } else if needs_items {
        return Ok(ServerItemPage::default());
    }
    if query.person_ids.is_some() {
        let mut enriched = Vec::with_capacity(rows.len());
        for row in rows {
            enriched.push(provider.enrich_visible_item(row).await);
        }
        rows = enriched;
    }
    Ok(select_page(rows, &query))
}

/// 默认实现的 NextUp：一次候选读 + 按 Media 分组选第一条未看。
///
/// provider 的根列表如果只有 series 级快照（旧式 provider），这里退回逐 series
/// 查询，因此**不承诺有界**；生产 provider 覆盖
/// [`MediaServerProvider::next_up_items`] 后一次聚合。
pub async fn legacy_next_up(
    provider: &(impl MediaServerProvider + ?Sized),
    user: UserId,
    series_id: Option<&str>,
    offset: usize,
    limit: usize,
) -> Result<ServerItemPage, String> {
    let requested = series_id.map(|id| id.replace('-', ""));
    let candidates = provider
        .list_visible_items(user, requested.as_deref())
        .await?;
    // 显式 SeriesId 与生产路径一致：不限制种类（电影也能返回）。缺省才只要 TV。
    let restrict_kind = series_id.is_none();
    let mut episodes = Vec::new();
    let mut expanded_media = HashSet::new();
    for item in candidates {
        if !item.is_series {
            if !restrict_kind || item.media.kind == MediaKind::Tv {
                expanded_media.insert(item.media.id);
                episodes.push(item);
            }
            continue;
        }
        if !expanded_media.insert(item.media.id) {
            continue;
        }
        let series_id = item.media.id.to_string().replace('-', "");
        let expanded = provider.list_visible_items(user, Some(&series_id)).await?;
        episodes.extend(expanded.into_iter().filter(|episode| {
            !episode.is_series && (!restrict_kind || episode.media.kind == MediaKind::Tv)
        }));
    }
    Ok(first_unwatched_per_media(episodes, offset, limit))
}

/// 候选按 Media 分组（保持候选顺序），每个 Media 取 (季, 集) 最小的未看条目，
/// 再整体分页。`total` 是全部命中的 Media 数。
pub fn first_unwatched_per_media(
    candidates: Vec<MediaItemSnapshot>,
    offset: usize,
    limit: usize,
) -> ServerItemPage {
    let mut order: Vec<domain::MediaId> = Vec::new();
    let mut groups: std::collections::HashMap<domain::MediaId, Vec<MediaItemSnapshot>> =
        std::collections::HashMap::new();
    for item in candidates {
        if item.is_series {
            continue;
        }
        let media_id = item.media.id;
        let group = groups.entry(media_id).or_default();
        if group.is_empty() {
            order.push(media_id);
        }
        group.push(item);
    }
    let mut next_up = Vec::new();
    for media_id in order {
        let mut group = groups.remove(&media_id).unwrap_or_default();
        group.sort_by_key(|item| {
            (
                item.row.season.unwrap_or_default(),
                item.row.episode.unwrap_or_default(),
            )
        });
        if let Some(episode) = group.into_iter().find(|item| !item.played) {
            next_up.push(episode);
        }
    }
    ServerItemPage::from_rows(next_up, offset, limit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn series_and_season_ids_are_episode_requests() {
        let mut query = ServerItemQuery::default();
        assert!(!query.wants_episodes());
        assert!(query.wants_series(), "缺省列表包含剧");
        query.series_id = Some("abc".into());
        assert!(query.wants_episodes(), "SeriesId 就是列分集");
        assert!(query.wants_series(), "SeriesId 不排除剧本身");
        query.include_item_types = Some("Series,Episode".into());
        assert!(query.wants_episodes());
        assert!(query.wants_series());
        query.include_item_types = Some("Movie".into());
        query.series_id = None;
        query.season_id = Some("season-1".into());
        assert!(query.wants_episodes(), "SeasonId 就是列分集");
        query.season_id = None;
        assert!(!query.wants_episodes());
        assert!(!query.wants_series());
    }
}
