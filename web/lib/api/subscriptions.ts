import { request } from "@/lib/http";
import type { MediaType } from "@/lib/media-types";

/** 后端统一响应信封 */
interface ApiEnvelope<T> {
  success: boolean;
  code: string;
  message: string;
  data: T;
}

async function unwrap<T>(promise: Promise<ApiEnvelope<T>>): Promise<T> {
  return (await promise).data;
}

/** 订阅状态（见 self.md tracking_state；completed 是前端派生值） */
export type SubscriptionStatus = "active" | "paused" | "completed";

/** 工单状态机（见后端 WantedStatus） */
export type WantedStatus = "wanted" | "grabbed" | "downloaded" | "imported";

/** 订阅拉取模式（见 self.md subscribe.fetch_mode） */
export type FetchMode = "search" | "rss" | "both";

/** 订阅覆盖范围（见 self.md coverage）。 */
export type SubscriptionCoverage =
  | { kind: "movie" }
  | { kind: "tv"; season: number; episode_from: number; episode_to?: number | null };

/** 媒体条目视图（见 self.md MediaView；id 与外部 id 均为字符串直通）。 */
export interface MediaView {
  id: string;
  kind: MediaType;
  title: string;
  year?: number | null;
  original_title?: string | null;
  tmdb_id?: string | null;
  douban_id?: string | null;
  tvdb_id?: string | null;
  bangumi_id?: string | null;
  anilist_id?: string | null;
  poster_url?: string | null;
}

/** 条目摘要（新契约 MediaView 形状；media_item_id 为 UUID 字符串）。 */
export interface SubscriptionMedia {
  media_item_id: string;
  kind: MediaType;
  tmdb_id: string | null;
  douban_id: string | null;
  title: string;
  original_title: string | null;
  year: number | null;
  poster_url: string | null;
  status: string | null;
}

/** 弹层季选择器的一行 */
export interface SeasonOverview {
  season_number: number;
  name: string;
  air_date: string | null;
  episode_count: number | null;
  /** 已播集数（air_date<=今天） */
  aired_count: number;
  /** 媒体库已有的集数（库存 H） */
  owned_count: number;
}

/** 豆瓣收敛歧义时的确认候选 */
export interface ResolveCandidate {
  tmdb_id: string;
  /** 选定该候选后用于创建订阅的稳定引用 */
  title_ref: string;
  title: string;
  original_title: string;
  year: number | null;
  poster_url: string | null;
}

/** 订阅预检结果：ready 可直接渲染弹层 / ambiguous 先选候选 / not_found 无法订阅 */
export interface PrepareResult {
  status: "ready" | "ambiguous" | "not_found";
  media: SubscriptionMedia | null;
  seasons: SeasonOverview[];
  existing_subscription_id: string | null;
  /** 电影：媒体库里已有本片（弹层提示，不拦订阅） */
  movie_owned: boolean;
  /**
   * 建议预勾选的季号：豆瓣把剧集按季拆条目，点进「中餐厅 第十季」要订的就是那一季。
   * 服务端只在条目确实季专属时给出（且季号是 TMDB 的，与豆瓣季号未必相同）；
   * 为空表示无结论，按「全部已播正季」的原默认规则勾选。
   */
  suggested_seasons: number[];
  candidates: ResolveCandidate[];
}

export interface SubscriptionProgress {
  total: number;
  imported: number;
  missing: number;
  grabbing: number;
  downloaded: number;
}

/** 单元的洗版派生状态（见后端 WantedUpgradeView）；标签由后端生成。 */
export interface WantedUpgrade {
  /** 是否洗版中（可证明低于目标且未熔断） */
  active: boolean;
  /** 当前版本档位标签（如「1080p WEB-DL」） */
  current_label: string;
  /** 洗版目标档位标签（如「1080p Remux」） */
  target_label: string;
  /** 已洗版搜索次数 */
  search_attempts: number;
  /** 无法确认档位（证明不了低于目标也证明不了达标）：不参与自动洗版，可手动选种替换 */
  indeterminate: boolean;
}

/** 订阅（见 self.md Subscription）。 */
export interface Subscription {
  id: string;
  media: MediaView;
  user_id: string;
  coverage: SubscriptionCoverage;
  fetch_mode: FetchMode;
  filter_id: string | null;
  wash_cut: boolean;
  wash_cut_filter_id?: string | null;
  /** 洗版时保留被替换的旧版本（同时留 1080p 与 4K）。 */
  keep_old_versions: boolean;
  full_season_pack: boolean;
  downloader_id?: string | null;
  library_id?: string | null;
  /** Explicit target availability; never silently resets library_id. */
  target_library_status: "ready" | "deleting" | "missing";
  target_library_error: string | null;
  /** completed：后端 M4a 判定已全部入库（非洗版有界订阅），搜索跳过索引站。 */
  tracking_state: "active" | "paused" | "completed";
  follow_future: boolean;
  /** Seconds between automatic Site searches. Default 1800. */
  search_interval_secs: number;
  progress: SubscriptionProgress;
  created_at: string;
  updated_at: string;
}

/** 订阅首页的一行待入库摘要（今天，或今天没有安排时窗口内最近的一天）。 */
export interface TodaySubscriptionArrival {
  subscription_id: string;
  wanted_id: string;
  media_title: string;
  media_kind: "movie" | "tv";
  season_number: number;
  episode_number: number;
  /** imported = 当日已入库；grabbed = 在途下载中 */
  status: Extract<WantedStatus, "wanted" | "grabbed" | "downloaded"> | "imported";
  air_date: string | null;
  /** 预计入库/播出的站点日历日（YYYY-MM-DD），用于展示日期。 */
  expected_day: string;
  /** expected_day 距今天几天（0=今天）。站点日历口径由后端算好，前端不复制时区规则。 */
  days_ahead: number;
  release_forecast: ReleaseForecast | null;
  /** 后端按站点游标和礼貌间隔计算的下一次有效预测探测时间。 */
  next_probe_at: string | null;
  info_hash: string | null;
  grabbed_at: string | null;
  downloaded_at: string | null;
  estimated_release_to_import_minutes: number;
  estimated_download_to_import_minutes: number;
}

export interface ReleaseForecastSite {
  site_id: string;
  predicted_at: string;
  window_start: string;
  window_end: string;
  lag_minutes: number;
  coverage_count: number;
  probe_times: string[];
}

/** 单集资源发布时间预测快照；它是可重算的调度结论，不是新的观测事实源。 */
export interface ReleaseForecast {
  version: number;
  generated_at: string;
  target_air_date: string;
  predicted_at: string;
  window_start: string;
  window_end: string;
  confidence: "bootstrap" | "growing" | "stable" | "volatile";
  sample_count: number;
  cadence_days: number;
  basis_units: [number, number][];
  basis_torrent_row_ids: number[];
  sites: ReleaseForecastSite[];
  first: {
    generated_at: string;
    predicted_at: string;
    window_start: string;
    window_end: string;
    confidence: string;
    sample_count: number;
  };
}

/** 一集最近一次成功投递所用资源的发布→索引→提交时间链。 */
export interface ResourceTiming {
  site_id: string;
  torrent_id: string;
  publish_time: string | null;
  first_seen_at: string | null;
  submitted_at: string;
  publish_to_seen_seconds: number | null;
  seen_to_submit_seconds: number | null;
  publish_to_submit_seconds: number | null;
  dry_run: boolean;
}

export interface WantedItem {
  id: string;
  season_number: number;
  episode_number: number;
  status: WantedStatus;
  air_date: string | null;
  priority: number;
  next_search_at: string | null;
  search_attempts: number;
  last_search_at: string | null;
  release_forecast: ReleaseForecast | null;
  resource_timing: ResourceTiming | null;
  grabbed_at: string | null;
  downloaded_at: string | null;
  imported_at: string | null;
  /** 在途工单锚定的种子 hash；据此与 listActiveSubscriptionDownloads 的进度组对上 */
  info_hash: string | null;
  /** 单集履历注解：最近一次候选被拒原因（含站点与种子名）；null=从未被拒 */
  last_reject_reason: string | null;
  /** 单集履历注解：最近一次投递的种子（站点 · 标题）；null=未投递 */
  grab_title: string | null;
  /** 洗版派生状态；规则组未配洗版目标或单元未入库时为 null */
  upgrade: WantedUpgrade | null;
}

/** 订阅详情 = 订阅 + 工单明细（新契约详情接口不再带 wanted 列表，兼容字段为空数组）。 */
export interface SubscriptionDetail extends Subscription {
  wanted: WantedItem[];
}

/** 规则组过滤条件（见后端匹配器 RuleSetSpec）：全部键可缺省=不限。 */
export interface RuleSetSpec {
  /** 允许的分辨率；列表顺序即偏好顺序（排前面的评分更高） */
  resolutions?: string[];
  /** 允许的片源档（remux/blu-ray/web-dl/rip/tv）；顺序即偏好，与分辨率一样参与选优 */
  media_sources?: string[];
  video_codecs?: string[];
  /** 流媒体平台白名单（规范值，见 lib/platforms.ts）；空=不限。与制作组各自独立生效 */
  platforms?: string[];
  /** 流媒体平台黑名单（规范值）；命中即排除，优先于白名单 */
  platforms_block?: string[];
  release_groups_allow?: string[];
  release_groups_block?: string[];
  /** HDR 白名单兼偏好序（DV/HDR10+/HDR10/HLG/SDR，SDR=未标注）；任一命中即过 */
  hdr_levels?: string[];
  /** HDR 黑名单：资源带其中任一格式即排除 */
  hdr_block?: string[];
  /** [兼容层] 旧的 HDR 三态；hdr_levels/hdr_block 为空时后端才消费它 */
  hdr?: "any" | "require" | "forbid";
  /** [兼容层] 旧的 DV 三态；同上 */
  dv?: "any" | "require" | "forbid";
  free_only?: boolean;
  min_seeders?: number | null;
  /** 体积区间按「每集均摊」评估：整季包用总体积 ÷ 集数比较 */
  size_min_mb?: number | null;
  size_max_mb?: number | null;
  exclude_hr?: boolean;
  hr_unknown_policy?: "lenient" | "strict";
  /** 要求的字幕语言（BCP 47，任一命中即过；"zh" 含简/繁/未标简繁）；空=不限 */
  subtitle_languages_require?: string[];
  /** 要求的音轨语言（cmn=国语、yue=粤语…）；空=不限 */
  audio_languages_require?: string[];
  /** 洗版目标片源档；缺省=不洗版（docs/design/quality-upgrade.md §3.2） */
  upgrade_source?: "web-dl" | "blu-ray" | "remux" | null;
  /** 洗版目标分辨率；缺省=分辨率偏好首选（都缺省则 1080p），须在 resolutions 内 */
  cutoff_resolution?: string | null;
  /** 洗到新版本后保留旧版本（多版本共存，收藏家模式）；缺省=旧版本进回收站 */
  upgrade_keep_old?: boolean;
  /** 参与洗版比较的维度及优先级（顺序即位次）；缺省 ["resolution","source"] */
  upgrade_ladder?: string[];
  /** 按优先级排列的首选站点；首选站点资源优先，同站点内仍按质量评分。 */
  sites?: string[];
}

/** 规则组原子（见 self.md RuleSet.atoms；exclude 黑名单原子命中即排除）。 */
export interface RuleSetAtom {
  kind:
    | "resolution"
    | "source"
    | "video_codec"
    | "free"
    | "hr"
    | "title_match"
    | "hdr"
    | "size"
    | "min_seeders"
    | "subtitle_language"
    | "audio_language"
    | "site"
    | "wash_target"
    | "upgrade_ladder";
  value: string | null;
  priority: number;
  exclude?: boolean;
}

/** 规则组（见 self.md RuleSet；旧 spec 对象已折叠为 atoms 原子列表）。 */
export interface RuleSet {
  id: string;
  name: string;
  is_default: boolean;
  keep_old_versions?: boolean;
  atoms: RuleSetAtom[];
}

export interface SubscriptionTargetPreviewPayload {
  /** Discover 返回的影视条目稳定引用 */
  title_ref: string;
}

export interface CreateSubscriptionPayload {
  /** Discover 返回或歧义候选确认后的稳定引用 */
  title_ref: string;
  /** 从豆瓣候选改选 TMDB 条目时保留原始豆瓣身份 */
  source_title_ref?: string | null;
  selected_seasons?: number[];
  follow_future?: boolean;
  rule_set_id?: number | null;
  /** 入库目标库；缺省用该类型的默认库 */
  library_id?: string | number | null;
  wash_cut?: boolean;
  wash_cut_filter_id?: string | number | null;
  keep_old_versions?: boolean;
}

export interface CreateSubscriptionResult {
  subscription: SubscriptionDetail;
  /** 管理员可见的下载与入库路由预检；成员调用时为空 */
  download_routing: DispatchPreview | null;
}

/** 投递路由预检（见 schemas.subscription.DispatchPreviewView）。 */
export interface DispatchPreview {
  /** watch=投监听导入目录 / inplace=直接下载进库 / downloader_default=下载器默认目录 */
  mode: "watch" | "inplace" | "downloader_default";
  /** 本机视角的投递基底目录 */
  path: string | null;
  /**
   * 条目目录的完整路径预览（后端按生效的命名模板渲染）。
   * 展示"直接下载到 …"时用它，**不要**自己拼 `${title} (${year})`：
   * 命名模板可全局/按库自定义，自己拼会与真实落点不一致。
   */
  entry_dir: string | null;
  /** 命中自定义目录规则时的整理落点：完成后整理到该目录（不直接入库），外部流转回库根后入账 */
  staging_path: string | null;
  /** 解析出的目标库（未选库时=收藏范围路由的结论，弹窗据此预选） */
  library_id: number | null;
  library_name: string | null;
  downloader_name: string | null;
  /** 收藏范围路由结论：true=命中声明库 / false=默认库兜底；未走路由为 null */
  route_matched: boolean | null;
  /** 路由理由（中文整句，徽标直接展示） */
  route_reason: string | null;
  /** 被收藏范围路由命中的库所配置的默认规则组；未配置或未走路由时为空。 */
  default_filter_id?: string | null;
  /** 按当前配置投递能否顺利入库 */
  ok: boolean;
  /** 不 ok 时的中文指引 */
  warning: string | null;
}

/**
 * 投递路由预检：订阅弹窗选库时调用，预演下载会落到哪、能否自动入库。
 * `libraryId` 为 null 且带 `tmdbId` 时由后端按收藏范围路由选库并返回理由。
 *
 * POST /subscriptions/download-routing-preview。
 */
export function previewSubscriptionDownloadRouting(
  kind: string,
  libraryId: number | null,
  tmdbId?: string | null,
  identity?: { title?: string | null; year?: number | null },
): Promise<DispatchPreview> {
  return unwrap(
    request<ApiEnvelope<DispatchPreview>>("/subscriptions/download-routing-preview", {
      method: "POST",
      body: JSON.stringify({
        kind,
        library_id: libraryId != null ? String(libraryId) : null,
        tmdb_id: tmdbId ?? null,
        title: identity?.title ?? null,
        year: identity?.year ?? null,
      }),
    }),
  );
}

/** 订阅链路体检里的一段检查结论。 */
export interface PipelineCheck {
  key: string;
  /** 段落名（「下载器」「路径映射」「硬链接同盘」…） */
  label: string;
  /** ok=正常 / warn=能转但降级 / error=会失败，必须修 */
  status: "ok" | "warn" | "error";
  /** 中文事实陈述，直接展示 */
  detail: string;
  /** 修复去处：设置分区 id（downloaders/import-watch）或 libraries（媒体库页） */
  fix_section: string | null;
}

/** 一个库的完整入库链路结论。 */
export interface LibraryPipeline {
  library_id: number;
  /** 新版响应字段；旧的 automation-readiness 响应使用 name。 */
  library_name?: string;
  name?: string;
  kind: "movie" | "tv";
  is_default: boolean;
  /** watch=投监听目录 / inplace=直下库根 / downloader_default=下载器默认目录 */
  mode: "watch" | "inplace" | "downloader_default";
  path: string | null;
  /** 库主根（入库节点的落点展示） */
  library_root: string | null;
  /** 命中自定义目录规则时的整理落点：非空时转移段不直接入库，外部流转后回库根入账 */
  staging_path: string | null;
  status: "ok" | "warn" | "error";
  /** 「订阅命中本库会发生什么」的一句话叙事（全绿时的正向可预期） */
  narrative: string;
  checks: PipelineCheck[];
}

/** 修复卡里的一个可选修法。 */
export interface FixOption {
  title: string;
  /** 为什么这么做 / 适合谁（帮用户在多个选项间取舍；可为空串） */
  why: string;
  /** 具体做什么，含建议值 */
  steps: string;
  /** 修复去处（与 PipelineCheck.fix_section 同词表，前端映射到路由） */
  fix_section: string;
  fix_label: string;
  /** 跳转预填参数：目标设置页读取后自动填表单，免去誊写路径 */
  fix_params: Record<string, string> | null;
}

/** 按根因聚合的问题卡：一个根因 = 一张卡，不随受影响的库数膨胀。 */
export interface PipelineIssue {
  /** 与被聚合检查项的 PipelineCheck.key 同词表（库卡片据此隐藏重复红项） */
  key: string;
  status: "warn" | "error";
  title: string;
  detail: string;
  affected_libraries: string[];
  options: FixOption[];
}

/** 订阅链路体检整体结论（订阅设定页与订阅列表警示横幅共用）。 */
export interface PipelineHealth {
  /** 整体状态：库链路 + 全局段（站点/下载器）的最坏值 */
  status: "ok" | "warn" | "error";
  /** 链路有 error 的库数 */
  error_count: number;
  warn_count: number;
  /** 资源搜索段（全局，链路第一环） */
  site_check: PipelineCheck;
  /** 是否有可用的默认下载器 */
  downloader_ok: boolean;
  /** 是否配置过站点（无论当前可用与否）——开局清单只看它 */
  sites_configured: boolean;
  /** 是否配置过下载器（同上语义） */
  downloaders_configured: boolean;
  /** 按根因聚合的问题卡（error 在前）——置顶展示为「需要处理 N 件事」 */
  issues: PipelineIssue[];
  libraries: LibraryPipeline[];
}

/**
 * 订阅链路体检：逐库预演「投递 → 转移 → 入库」，与真实投递同源判定。
 * 新契约返回 `{status, sites_configured, rule_sets_configured, downloaders_configured, issues}`，
 * 字段子集与旧 PipelineHealth 不一致处由组件适配阶段收敛。
 */
export function checkSubscriptionAutomationReadiness(
  init?: RequestInit,
): Promise<PipelineHealth> {
  return unwrap(request<ApiEnvelope<PipelineHealth>>("/subscriptions/automation-readiness", init));
}

/**
 * 订阅预检：按 title_ref（tmdb:movie:603 / 裸标题）解析媒体、季集、已有订阅。
 * 后端真实实现（POST /subscriptions/title-preview）；douban 引用暂无法解析。
 */
export function previewSubscriptionTitle(
  payload: SubscriptionTargetPreviewPayload,
): Promise<PrepareResult> {
  return unwrap(
    request<ApiEnvelope<PrepareResult>>("/subscriptions/title-preview", {
      method: "POST",
      body: JSON.stringify({ title_ref: payload.title_ref }),
    }),
  );
}

import { createSubscriptionBody } from "../subscription-form";

/** 创建订阅（同条目重复订阅幂等返回已有）。后端接受 title_ref 直接解析。 */
export function createSubscription(
  payload: CreateSubscriptionPayload,
): Promise<SubscriptionDetail> {
  const body = createSubscriptionBody(payload);
  return unwrap(
    request<ApiEnvelope<Subscription>>("/subscriptions", {
      method: "POST",
      body: JSON.stringify(body),
    }),
  ).then((sub) => ({ ...sub, wanted: [] }));
}

/** 订阅列表（含工单进度）。kind 缺省返回全部。 */
export function listSubscriptions(
  kind?: MediaType,
  init?: RequestInit,
): Promise<Subscription[]> {
  const query = kind ? `?kind=${kind}` : "";
  return unwrap(request<ApiEnvelope<Subscription[]>>(`/subscriptions${query}`, init));
}

/**
 * 今日可能入库（真实）：在途（grabbed）与当日入库（imported）的订阅。
 * 后端按 pending 与 ledger 修改时间计算；无预告/ETA 时相应字段为 null。
 */
export function listTodaySubscriptionArrivals(
  init?: RequestInit,
): Promise<TodaySubscriptionArrival[]> {
  return unwrap(
    request<ApiEnvelope<TodayArrivalItem[]>>("/subscriptions/today-arrivals", init),
  ).then((items) =>
    items.map((item) => ({
      subscription_id: item.subscription_id,
      wanted_id: "",
      media_title: item.title,
      media_kind: item.coverage?.kind === "tv" ? "tv" : "movie",
      season_number: item.coverage && "season" in item.coverage ? item.coverage.season : 0,
      episode_number: 0,
      status: item.status === "imported" ? "imported" : item.status === "grabbed" ? "grabbed" : "wanted",
      air_date: null,
      expected_day: "",
      days_ahead: 0,
      release_forecast: null,
      next_probe_at: null,
      info_hash: item.info_hash ?? null,
      grabbed_at: item.grabbed_at ?? null,
      downloaded_at: null,
      estimated_release_to_import_minutes: 0,
      estimated_download_to_import_minutes: 0,
    })),
  );
}

/** 新契约 today-arrivals 的单条条目。 */
interface TodayArrivalItem {
  subscription_id: string;
  title: string;
  coverage: SubscriptionCoverage | null;
  eta: string | null;
  status?: "grabbed" | "imported" | "wanted";
  info_hash?: string | null;
  grabbed_at?: string | null;
}

/** 订阅详情（含工单明细 wanted：单元状态 + 履历，后端现算）。 */
export function getSubscription(id: string): Promise<SubscriptionDetail> {
  return unwrap(request<ApiEnvelope<Subscription & { wanted?: WantedItem[] }>>(`/subscriptions/${id}`)).then(
    (sub) => ({ ...sub, wanted: sub.wanted ?? [] }),
  );
}

/** 修改订阅（季选择/自动续订/规则组，后端 diff 重算工单）。
 *  新契约 PATCH 只接受 fetch_mode/wash_cut/full_season_pack/downloader_id；
 *  旧字段按语义映射（rule_set_id→filter_id），其余被后端忽略。 */
export function updateSubscription(
  id: string,
  payload: {
    selected_seasons?: number[];
    follow_future?: boolean;
    rule_set_id?: string | number | null;
    /** 换入库目标库；显式传 null=清除指定、改回按默认库路由；缺省不变 */
    library_id?: string | number | null;
  },
): Promise<SubscriptionDetail> {
  const body: Record<string, unknown> = {};
  if (payload.selected_seasons) body.selected_seasons = payload.selected_seasons;
  if (payload.rule_set_id != null) body.filter_id = String(payload.rule_set_id);
  if (payload.follow_future !== undefined) body.follow_future = payload.follow_future;
  if (payload.library_id !== undefined) {
    body.library_id = payload.library_id === null ? null : String(payload.library_id);
  }
  return unwrap(
    request<ApiEnvelope<Subscription>>(`/subscriptions/${id}`, {
      method: "PATCH",
      body: JSON.stringify(body),
    }),
  ).then((sub) => ({ ...sub, wanted: [] }));
}

/** 立即搜索：缺口工单跳过冷却重新排队（暂停中/无可搜缺口时后端报可读错误）。 */
export function searchMissingSubscriptionResources(
  id: string,
): Promise<{ reset_count: number }> {
  return unwrap(
    request<ApiEnvelope<{ queued: boolean; reset_count: number }>>(
      `/subscriptions/${id}/missing-resource-searches`,
      { method: "POST" },
    ),
  ).then(({ reset_count }) => ({ reset_count }));
}

/** 一轮洗版体检里单个单元的结论（见 schemas.subscription.UpgradeRunUnitView）。 */
export interface UpgradeRunUnit {
  season_number: number;
  episode_number: number;
  /** upgradable=已排入立即搜索 / at_cutoff=已达目标 / in_flight=已在洗 /
   *  not_comparable=无法识别当前版本 / missing=缺失（照常走缺口下载） */
  state: "upgradable" | "at_cutoff" | "in_flight" | "not_comparable" | "missing";
  current_label: string | null;
  target_label: string;
}

/** 一轮洗版的体检报告（见 schemas.subscription.UpgradeRunView）。 */
export interface UpgradeRunReport {
  target_label: string;
  rule_set_id: number;
  /** 中文整句摘要，直接展示 */
  summary: string;
  counts: Record<UpgradeRunUnit["state"], number>;
  units: UpgradeRunUnit[];
}

/**
 * 触发一轮洗版：可升级/缺失的单元排入立即搜索，并返回单元级体检报告。
 * 后端按 facts/pending/规则组目标真实计算 counts 与 units。
 */
export function runSubscriptionUpgradeRound(
  id: string,
  ruleSetId?: number | null,
): Promise<UpgradeRunReport> {
  return unwrap(
    request<ApiEnvelope<UpgradeRunReport>>(`/subscriptions/${id}/upgrade-runs`, {
      method: "POST",
      body: JSON.stringify(ruleSetId != null ? { rule_set_id: ruleSetId } : {}),
    }),
  ).then((report) => ({
    target_label: report.target_label ?? "",
    rule_set_id: Number(report.rule_set_id ?? 0) || 0,
    summary: report.summary ?? "洗版轮已排队执行",
    counts: {
      upgradable: report.counts?.upgradable ?? 0,
      at_cutoff: report.counts?.at_cutoff ?? 0,
      in_flight: report.counts?.in_flight ?? 0,
      not_comparable: report.counts?.not_comparable ?? 0,
      missing: report.counts?.missing ?? 0,
    },
    units: report.units ?? [],
  }));
}

/** 手动选种的种子字段（搜索结果行原样回传，attrs 即搜索链路的服务端解析）。 */
export interface GrabPayload {
  site_id: string;
  torrent_id: string;
  title: string;
  subtitle?: string;
  category?: string | null;
  attrs?: Record<string, unknown> | null;
  download_url?: string | null;
  size_bytes?: number | null;
  seeders?: number | null;
  is_free?: boolean | null;
  hit_and_run?: boolean | null;
  publish_time?: string | null;
}

/**
 * 手动选种：把一条搜索结果直接投给订阅（跳过规则组过滤；身份匹配照常）。
 * 旧端点 `/subscriptions/{id}/selected-torrent-downloads` 已下线，改投
 * `/downloaders/submit` 完成实际下载。
 */
export function downloadSelectedTorrentForSubscription(
  id: string,
  payload: GrabPayload,
): Promise<{ units: { season_number: number; episode_number: number }[] }> {
  const body: Record<string, unknown> = {
    download_url: payload.download_url ?? "",
    ...(payload.torrent_id ? { torrent_id: payload.torrent_id } : {}),
    title: payload.title,
    subscribe_id: id,
    ...(payload.site_id ? { site_id: payload.site_id } : {}),
    ...(payload.size_bytes != null ? { size_bytes: payload.size_bytes } : {}),
  };
  return unwrap(
    request<ApiEnvelope<{ ok: boolean; save_path?: string | null; units?: { season_number: number; episode_number: number }[] }>>(
      "/downloaders/submit",
      {
        method: "POST",
        body: JSON.stringify(body),
      },
    ),
  ).then((resp) => ({ units: resp.units ?? [] }));
}

/**
 * 暂停 / 恢复订阅：PATCH /subscriptions/{id} `{tracking_state}`。
 */
export function setSubscriptionTrackingState(
  id: string,
  state: "active" | "paused",
): Promise<SubscriptionDetail> {
  return unwrap(
    request<ApiEnvelope<Subscription>>(`/subscriptions/${id}`, {
      method: "PATCH",
      body: JSON.stringify({ tracking_state: state }),
    }),
  ).then((sub) => ({ ...sub, wanted: [] }));
}

export function setSubscriptionFetchMode(
  id: string,
  fetch_mode: FetchMode,
): Promise<SubscriptionDetail> {
  return unwrap(
    request<ApiEnvelope<Subscription>>(`/subscriptions/${id}`, {
      method: "PATCH",
      body: JSON.stringify({ fetch_mode }),
    }),
  ).then((sub) => ({ ...sub, wanted: [] }));
}

export function setSubscriptionSearchInterval(
  id: string,
  search_interval_secs: number,
): Promise<SubscriptionDetail> {
  return unwrap(
    request<ApiEnvelope<Subscription>>(`/subscriptions/${id}`, {
      method: "PATCH",
      body: JSON.stringify({ search_interval_secs }),
    }),
  ).then((sub) => ({ ...sub, wanted: [] }));
}

/**
 * 洗版保留旧版本：PATCH /subscriptions/{id} `{keep_old_versions}`。
 */
export function setSubscriptionKeepOldVersions(
  id: string,
  enabled: boolean,
): Promise<SubscriptionDetail> {
  return unwrap(
    request<ApiEnvelope<Subscription>>(`/subscriptions/${id}`, {
      method: "PATCH",
      body: JSON.stringify({ keep_old_versions: enabled }),
    }),
  ).then((sub) => ({ ...sub, wanted: [] }));
}

/** 剧集自动续订：PATCH /subscriptions/{id} `{follow_future}`。 */
export function setSubscriptionFollowFuture(
  id: string,
  enabled: boolean,
): Promise<SubscriptionDetail> {
  return unwrap(
    request<ApiEnvelope<Subscription>>(`/subscriptions/${id}`, {
      method: "PATCH",
      body: JSON.stringify({ follow_future: enabled }),
    }),
  ).then((sub) => ({ ...sub, wanted: [] }));
}

/**
 * 成员停止关注（退订）；服务端保证不影响仍在追踪的其他成员。
 * 调用 DELETE /subscriptions/{id} 移除订阅。
 */
export function unsubscribeFromSubscription(id: string): Promise<Record<string, never>> {
  return unwrap(
    request<ApiEnvelope<{ deleted: boolean }>>(`/subscriptions/${id}`, {
      method: "DELETE",
    }),
  ).then(() => ({}));
}

/** 按季清理时被有意保留的跨季种子（整季包仍被保留的季使用）。 */
export interface RetainedCrossSeasonTorrent {
  title: string;
  /** 该种子覆盖到的季号；空 = 无从按季定位的存量数据 */
  seasons: number[];
}

/** 可一并清理的内容（见 schemas.subscription.SubscriptionRemovalPreviewView）。 */
export interface SubscriptionRemovalPreview {
  torrent_count: number;
  torrent_titles: string[];
  hit_and_run_count: number;
  library_file_count: number;
  library_bytes: number;
  recycle_retention_days: number;
  retained_cross_season: RetainedCrossSeasonTorrent[];
}

/** 联动清理选项；两项都不勾 = 什么都不删（默认）。 */
export interface SubscriptionRemovalOptions {
  deleteTorrents: boolean;
  deleteLibraryFiles: boolean;
}

/**
 * 确认弹窗打开时拉取：能一起删掉多少种子与媒体库文件。
 *
 * `seasons`：只看这几季（减季后的按季清理）；不传 = 整条退订的范围——后端按
 * 「这条订阅覆盖过的季」收口，不是条目下的一切。
 * 新契约只回 `{library_file_count, library_bytes, torrent_count}`，其余字段补默认。
 */
export function getSubscriptionRemovalPreview(
  id: string,
  seasons?: number[],
): Promise<SubscriptionRemovalPreview> {
  const query = seasons?.length
    ? `?${new URLSearchParams(seasons.map((s) => ["seasons", String(s)]))}`
    : "";
  return unwrap(
    request<ApiEnvelope<SubscriptionRemovalPreview>>(
      `/subscriptions/${id}/removal-preview${query}`,
    ),
  ).then((preview) => ({
    torrent_count: preview.torrent_count ?? 0,
    torrent_titles: preview.torrent_titles ?? [],
    hit_and_run_count: preview.hit_and_run_count ?? 0,
    library_file_count: preview.library_file_count ?? 0,
    library_bytes: preview.library_bytes ?? 0,
    recycle_retention_days: preview.recycle_retention_days ?? 0,
    retained_cross_season: preview.retained_cross_season ?? [],
  }));
}

/**
 * 清理已移出订阅范围的那几季的内容（减季后的可选收尾）。
 *
 * 订阅不会被删——它还在追别的季。后端会拒绝仍在范围内的季，并把退出那几季
 * 已入库的单元退回缺口（以后重新勾选该季会重新下载）。
 * 新契约只回执 `{queued}`，旧 cleanup_job_id 兼容字段置 null。
 */
export function cleanupSubscriptionSeasons(
  id: string,
  seasons: number[],
  options: SubscriptionRemovalOptions,
): Promise<{ cleanup_job_id: string | null }> {
  return unwrap(
    request<ApiEnvelope<{ queued: boolean }>>(`/subscriptions/${id}/season-cleanup`, {
      method: "POST",
      body: JSON.stringify({
        seasons,
        delete_torrents: options.deleteTorrents,
        delete_library_files: options.deleteLibraryFiles,
      }),
    }),
  ).then(() => ({ cleanup_job_id: null }));
}

/**
 * 管理员永久删除订阅与追踪工单。
 *
 * 默认不动任何已有内容；勾了联动清理时后端立刻返回并把删种子/回收文件交给
 * 后台任务（cleanup_job_id 可在任务中心查看进度）。
 * 新契约 DELETE 只回 `{deleted}`，无清理任务概念，cleanup_job_id 恒为 null。
 */
export function deleteSubscriptionPermanently(
  id: string,
  options?: SubscriptionRemovalOptions,
): Promise<{ cleanup_job_id: string | null }> {
  const query = new URLSearchParams({
    delete_torrents: String(options?.deleteTorrents ?? false),
    delete_library_files: String(options?.deleteLibraryFiles ?? false),
  });
  return unwrap(
    request<ApiEnvelope<{ deleted: boolean }>>(`/subscriptions/${id}?${query}`, {
      method: "DELETE",
    }),
  ).then(() => ({ cleanup_job_id: null }));
}

/** 订阅在途种子的实时下载快照（详情页轮询展示进度/速度/ETA）。 */
export interface SubscriptionDownload {
  info_hash: string;
  /** 下载器中的任务名；missing 时为空 */
  name: string | null;
  /** 0.0 ~ 1.0；missing 时为空 */
  progress: number | null;
  size_bytes: number | null;
  dlspeed_bytes: number | null;
  eta_seconds: number | null;
  /** missing = 种子已不在任何可用下载器中（救援巡检稍后会退回工单重找） */
  state: "downloading" | "stalled" | "paused" | "completed" | "error" | "missing" | "unknown";
  /** state 为 error 时下载器给出的可读原因（文件缺失 / 出错详情）；其余为 null */
  error_message: string | null;
  downloader_name: string | null;
  units: { season_number: number; episode_number: number }[];
}

/**
 * 订阅在途种子的实时下载进度（纯读快照，逐个查询下载器）。
 * 旧端点 `/subscriptions/{id}/active-downloads` 已下线，改读 `/downloaders/tasks`
 * 的待投递快照作为近似。
 */
export function listActiveSubscriptionDownloads(
  id: string,
  init?: RequestInit,
): Promise<SubscriptionDownload[]> {
  void id;
  return unwrap(
    request<ApiEnvelope<{ items: DownloadTaskShim[] }>>("/downloaders/tasks", init),
  ).then((snapshot) =>
    (snapshot.items ?? []).map((task) => ({
      info_hash: task.info_hash ?? "",
      name: task.name,
      progress: task.progress,
      size_bytes: task.size_bytes,
      dlspeed_bytes: task.dlspeed_bytes,
      eta_seconds: null,
      state: (task.state ?? "unknown") as SubscriptionDownload["state"],
      error_message: task.error_message ?? null,
      downloader_name: task.downloader_name,
      units: [],
    })),
  );
}

/** /downloaders/tasks 条目的最小切片（避免与 downloaders.ts 类型耦合）。 */
interface DownloadTaskShim {
  info_hash?: string | null;
  name: string | null;
  progress: number | null;
  size_bytes: number | null;
  dlspeed_bytes: number | null;
  state: string | null;
  error_message?: string | null;
  downloader_name: string | null;
}

/** 订阅活动记录：message 是完整中文句子，时间线直接展示。 */
export interface SubscriptionActivity {
  id: string;
  type:
    | "created"
    | "adjusted"
    | "paused"
    | "resumed"
    | "completed"
    | "reopened"
    | "searched"
    | "match_accepted"
    | "match_rejected"
    | "grabbed"
    | "dispatch_failed"
    | "wanted_added"
    | "downloaded"
    | "imported"
    | "import_failed"
    | "download_stalled"
    | "replacement_searched"
    | "replacement_trial"
    | "replacement_promoted"
    | "replacement_cleanup"
    | "upgrade_grabbed"
    | "upgraded"
    | "upgrade_verify_failed"
    | "spec_mismatch";
  message: string;
  payload: Record<string, unknown>;
  created_at: string;
  /** 管线类活动所属工单；生命周期变更、搜索轮次等订阅级活动为 null */
  wanted_item_id: string | null;
}

/**
 * 订阅活动时间线（后端按 Job 记录 + facts 推导事件类型与中文消息）。
 */
export function listSubscriptionActivities(
  id: string,
  limit = 100,
): Promise<SubscriptionActivity[]> {
  return unwrap(
    request<ApiEnvelope<{ items: SubscriptionActivityItem[]; total: number }>>(
      `/subscriptions/${id}/activities?limit=${limit}`,
    ),
  ).then((body) =>
    (body.items ?? []).map((item) => ({
      id: item.id ?? "",
      type: (item.type ?? "searched") as SubscriptionActivity["type"],
      message: item.message ?? [item.kind, item.status, item.error].filter(Boolean).join(" · "),
      payload: { ...item },
      created_at: item.started_at ?? item.finished_at ?? "",
      wanted_item_id: null,
    })),
  );
}

/** 新契约 activities 的单条事件（type/message 由后端给出）。 */
interface SubscriptionActivityItem {
  id?: string;
  type?: string;
  message?: string;
  kind: string;
  status: string;
  started_at: string | null;
  finished_at: string | null;
  error: string | null;
}

/** 规则组列表（首次访问后端自动创建默认组）。 */
export function listRuleSets(init?: RequestInit): Promise<RuleSet[]> {
  return unwrap(request<ApiEnvelope<RuleSet[]>>("/rule-sets", init));
}

/** 把旧 RuleSetSpec（偏好列表/布尔）折叠为新契约的 atoms 原子列表。 */
export function specToAtoms(spec: RuleSetSpec): RuleSetAtom[] {
  const atoms: RuleSetAtom[] = [];

  // 分辨率按画质梯队赋有业务意义的分数（2160p=100, 1080p=70, 720p=50），绝对不能从 0 起算！
  const resolutionWeights: Record<string, number> = {
    "2160p": 100,
    "4k": 100,
    "1080p": 70,
    "1080i": 65,
    "720p": 50,
    "480p": 30,
  };
  for (const resolution of spec.resolutions ?? []) {
    const p = resolutionWeights[resolution.toLowerCase()] ?? 60;
    atoms.push({ kind: "resolution", value: resolution, priority: p });
  }

  // 来源档位权重（Remux / BluRay 优先于 WEB-DL）
  const sourceWeights: Record<string, number> = {
    remux: 90,
    bluray: 85,
    "blu-ray": 85,
    "web-dl": 70,
    webrip: 60,
    hdtv: 50,
  };
  for (const source of spec.media_sources ?? []) {
    const p = sourceWeights[source.toLowerCase()] ?? 50;
    atoms.push({ kind: "source", value: source, priority: p });
  }

  if (spec.free_only) atoms.push({ kind: "free", value: null, priority: 50 });
  if (spec.exclude_hr) atoms.push({ kind: "hr", value: null, priority: 100, exclude: true });

  // 视频编码白名单（按画质阶梯赋优先级）
  const codecWeights: Record<string, number> = {
    av1: 95,
    hevc: 90,
    "h.265": 90,
    h265: 90,
    x265: 90,
    avc: 70,
    "h.264": 70,
    h264: 70,
    x264: 70,
  };
  for (const codec of spec.video_codecs ?? []) {
    const p = codecWeights[codec.toLowerCase()] ?? 60;
    atoms.push({ kind: "video_codec", value: codec, priority: p });
  }

  // 平台白名单（作为 title_match 包含匹配）
  for (const platform of spec.platforms ?? []) {
    atoms.push({ kind: "title_match", value: platform, priority: 60 });
  }

  // 制作组白名单
  for (const group of spec.release_groups_allow ?? []) {
    atoms.push({ kind: "title_match", value: group, priority: 60 });
  }

  // HDR 白名单（兼偏好序）
  const hdrWeights: Record<string, number> = {
    dovi: 100,
    dv: 100,
    "dolby vision": 100,
    "hdr10+": 90,
    hdr10: 80,
    hdr: 70,
  };
  for (const level of spec.hdr_levels ?? []) {
    const p = hdrWeights[level.toLowerCase()] ?? 60;
    atoms.push({ kind: "hdr", value: level, priority: p });
  }

  // 体积区间：单边缺省用 0（无下限）/ 空（无上限）
  if (spec.size_min_mb != null || spec.size_max_mb != null) {
    atoms.push({
      kind: "size",
      value: `${spec.size_min_mb ?? 0}-${spec.size_max_mb ?? ""}`,
      priority: 50,
    });
  }
  if (spec.min_seeders != null) {
    atoms.push({ kind: "min_seeders", value: String(spec.min_seeders), priority: 50 });
  }
  for (const lang of spec.subtitle_languages_require ?? []) {
    atoms.push({ kind: "subtitle_language", value: lang, priority: 50 });
  }
  for (const lang of spec.audio_languages_require ?? []) {
    atoms.push({ kind: "audio_language", value: lang, priority: 50 });
  }
  const preferredSites = spec.sites ?? [];
  preferredSites.forEach((site, index) => {
    // Atom priority encodes the tier: earlier sites must strictly outrank later ones.
    atoms.push({ kind: "site", value: site, priority: (preferredSites.length - index) * 50 });
  });
  // 洗版目标档位（规则面板「洗到哪一档」）：cutoff_resolution + upgrade_source → wash_target
  const upgradeParts: string[] = [];
  if (spec.cutoff_resolution) upgradeParts.push(spec.cutoff_resolution);
  if (spec.upgrade_source) upgradeParts.push(spec.upgrade_source);
  if (upgradeParts.length > 0) {
    atoms.push({ kind: "wash_target", value: upgradeParts.join(" "), priority: 50 });
  }
  // 洗版比较维度顺序（顺序即位次）
  if ((spec.upgrade_ladder ?? []).length > 0) {
    atoms.push({ kind: "upgrade_ladder", value: spec.upgrade_ladder!.join(","), priority: 50 });
  }
  // 黑名单 → exclude 原子（命中即排除；平台/制作组标记通常可见于标题）
  for (const block of spec.platforms_block ?? []) {
    atoms.push({ kind: "title_match", value: block, priority: 100, exclude: true });
  }
  for (const block of spec.release_groups_block ?? []) {
    atoms.push({ kind: "title_match", value: block, priority: 100, exclude: true });
  }
  for (const block of spec.hdr_block ?? []) {
    atoms.push({ kind: "hdr", value: block, priority: 100, exclude: true });
  }
  return atoms;
}

export function createRuleSet(name: string, spec: RuleSetSpec): Promise<RuleSet> {
  return unwrap(
    request<ApiEnvelope<RuleSet>>("/rule-sets", {
      method: "POST",
      body: JSON.stringify({
        name,
        atoms: specToAtoms(spec),
        keep_old_versions: spec.upgrade_keep_old ?? false,
      }),
    }),
  );
}

/** 更新规则组（只影响之后的匹配评估，不追溯已投递的工单）。 */
export function updateRuleSet(
  id: string,
  name: string,
  spec: RuleSetSpec,
  overrideAtoms?: RuleSetAtom[],
): Promise<RuleSet> {
  return unwrap(
    request<ApiEnvelope<RuleSet>>(`/rule-sets/${id}`, {
      method: "PATCH",
      body: JSON.stringify({
        name,
        atoms: overrideAtoms ?? specToAtoms(spec),
        keep_old_versions: spec.upgrade_keep_old ?? false,
      }),
    }),
  );
}

/** 设为默认规则组（新订阅未指定规则组时使用；不改已有订阅的挂靠）。 */
export function setDefaultRuleSet(id: string): Promise<RuleSet> {
  return unwrap(
    request<ApiEnvelope<{ default_rule_set_id: string }>>("/rule-sets/default", {
      method: "PUT",
      body: JSON.stringify({ id }),
    }),
  ).then((result) => ({
    id: result.default_rule_set_id,
    name: "",
    is_default: true,
    atoms: [],
  }));
}

/** 删除规则组（默认组与被订阅引用的组后端会拒绝，错误信息可直接展示）。 */
export function deleteRuleSet(id: string): Promise<void> {
  return unwrap(
    request<ApiEnvelope<{ deleted: boolean }>>(`/rule-sets/${id}`, { method: "DELETE" }),
  ).then(() => undefined);
}

export interface ReleaseForecastDay {
  date: string;
  weekday: number;
  count: number;
}

export async function fetchReleaseForecast(
  id: string,
): Promise<{ samples: number; days: ReleaseForecastDay[] }> {
  const envelope = await request<ApiEnvelope<{ samples: number; days: ReleaseForecastDay[] }>>(
    `/subscriptions/${id}/release-forecast`,
  );
  return envelope.data;
}
