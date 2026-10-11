import { request, resolveRequestUrl } from "@/lib/http";
import type { LibraryKind, MediaType } from "@/lib/media-types";
import { waitForLibraryRemoval, type RemovalWaitOptions } from "@/lib/library-removal";

export type { LibraryKind, MediaType };

/** 后端统一响应信封（自有 API：{ ok, data }，见 docs/api-contracts/self.md） */
interface ApiEnvelope<T> {
  ok: boolean;
  data: T;
}

async function unwrap<T>(promise: Promise<ApiEnvelope<T>>): Promise<T> {
  return (await promise).data;
}

// ---------------------------------------------------------------------------
// 自有 API 契约 DTO（docs/api-contracts/self.md §library / media）
// id 一律 UUID 字符串。
// ---------------------------------------------------------------------------

/** 库统计：真实台账统计。 */
export interface LibraryStats {
  item_count: number;
  file_count: number;
  total_size_bytes: number;
  strm_file_count?: number;
}

/** 媒体库（自契约 Library 的投影；组件引用名保持旧名 MediaLibrary）。 */
export interface MediaLibrary {
  id: string;
  name: string;
  /** 每库单一形态（movie / tv / video），创建后不可改 */
  kind: LibraryKind;
  /** 根路径列表（绝对路径），第一个为主根 */
  root_paths: string[];
  /** 是否为该类型的默认库 */
  is_default: boolean;
  /** 成员可见范围：everyone / selected */
  access_mode: LibraryAccessMode;
  /** 任一根目录在磁盘上不存在（被删/未挂载）——不能当「空库」展示 */
  root_missing: boolean;
  /** 超管浏览范围是否包含此库 */
  admin_visible: boolean;
  /** access_mode=selected 时可见的成员 id */
  member_ids: string[];
  /** 是否开启片头片尾标记检测 */
  detect_intros?: boolean;
  /** 是否开启音频声纹比对（针对无章节剧集） */
  enable_fingerprint?: boolean;
  /** 媒体库封面 URL（若已生成或用户上传） */
  cover_url?: string | null;
  /** 收藏范围规则 */
  match_rules?: MatchRule[];
  /** 关联的默认规则组 ID（新剧入库继承该规则组） */
  default_filter_id?: string | null;
  /** 是否开启实时目录监控 */
  realtime_watch?: boolean;
  /** 缺图时是否从视频抓帧生成封面 */
  generate_thumbnails?: boolean;
  /** 是否生成章节场景图 */
  extract_chapter_images?: boolean;
  /** 是否在首页展示排除此库 */
  exclude_from_home?: boolean;
  /** 是否按系列自动生成合集 */
  auto_series_collections?: boolean;
  /** 持久化删除状态；刷新或重新进入页面后继续跟踪。 */
  removal?: LibraryRemoval | null;
  /** 库存统计快照 */
  stats: LibraryStats;
}

/** 库内一个媒体条目的库存聚合（单库海报墙的一格）。 */
export interface LibraryItem {
  media_item_id: string;
  kind: LibraryKind;
  /** 这一格属于哪个库 */
  library_id: string;
  title: string;
  year: number | null;
  poster_url: string | null;
  backdrop_url?: string | null;
  file_count: number;
  /** 在库的季号列表（电影为空） */
  seasons: number[];
  /** 去重的 (季,集) 单元数（电影为 0） */
  episode_count: number;
  /** 该条目的文件是否已从磁盘消失（核验打标） */
  missing: boolean;
}

/** 条目详情里的一个物理文件（一个版本 / 一集）。 */
export interface LibraryItemFile {
  file_id: string;
  path: string;
  season: number | null;
  episode: number | null;
  resolution: string | null;
  codec: string | null;
  hdr: string | null;
  quality_source: string | null;
  /** ffprobe 抽取的音轨 / 字幕轨（file_meta 缓存；未探测为空数组） */
  audio_tracks?: AudioStream[];
  subtitle_tracks?: SubtitleStream[];
  /** ffprobe 抽取的主视频流信息（file_meta 缓存；未探测为 null） */
  video_track?: VideoTrackInfo | null;
  /** 是否已探测完成（false = 后台探测中，前端显示「探测中」并轮询） */
  probed?: boolean;
  /** 后台仍在排队或探测，包括流信息完成后的声纹阶段。 */
  probe_queued?: boolean;
  probe_stages?: {
    metadata?: ProbeStageView | null;
    intro?: ProbeStageView | null;
    outro?: ProbeStageView | null;
  } | null;
}

export interface ProbeStageView {
  status:
    | "pending"
    | "queued"
    | "running"
    | "succeeded"
    | "failed"
    | "not_applicable"
    | "cancelled";
  failure_count: number;
  next_retry_at_ms: number | null;
  error_kind: string | null;
}

/** 条目详情：基本信息 + 逐文件真实介质规格。 */
export interface LibraryItemCastMember {
  name: string;
  role: string | null;
  /** TMDB person id：非空时演职员可点进人物页 */
  tmdb_person_id: number | null;
  /** 人物头像（TMDB profile path 经图片代理） */
  avatar_url: string | null;
}

export interface LibraryItemDetail {
  media_item_id: string;
  kind: LibraryKind;
  title: string;
  year: number | null;
  original_title: string | null;
  tmdb_id: string | null;
  poster_url: string | null;
  /** 条目目录 NFO 刮削产物（overview/评分/片长/类型/演职员），缺失为 null/空 */
  overview: string | null;
  rating: string | null;
  runtime_minutes: string | null;
  genres: string[];
  cast: LibraryItemCastMember[];
  /** 本地 fanart.jpg 背景图（/fanart/{ledgerId}） */
  backdrop_url: string | null;
  files: LibraryItemFile[];
}

/** 剧集分集区的一集。 */
export interface LibraryEpisode {
  episode_number: number;
  name: string | null;
  overview: string | null;
  air_date: string | null;
  still_url: string | null;
  /** 该集有在位文件；false=缺集或文件缺失（置灰展示） */
  owned: boolean;
  /** 该集的台账文件 id */
  file_ids: string[];
  /** 当前观看者上次看到的位置（毫秒）；0=没看过或已重置 */
  position_ms: number;
  /** 当前观看者已看完该集 */
  played: boolean;
  /** 观看进度 1~99；已看完由 played 表达，不给百分比 */
  progress_percent: number | null;
}

/** 一季的分集清单。 */
export interface SeasonEpisodes {
  season_number: number;
  episodes: LibraryEpisode[];
}

/** 海报墙 A-Z 索引条的一档。 */
export interface LibraryIndexEntry {
  /** 档名：按标题排序是首字母 A-Z；按内容时间排序是月份；按评分排序是评分档 */
  initial: string;
  count: number;
  /** 该档第一格的位置——即 listLibraryItems 的 offset 取值 */
  offset: number;
}

/** 筛选面板里的一个候选值（计数已排除本维自身的条件）。 */
export interface FacetValue {
  value: string;
  label: string;
  /** 在**其他维度**已选条件下勾上本值还剩几部；0 的照常返回，前端置灰不可点 */
  count: number;
}

/** 一次筛选下的全部候选值与计数。 */
export interface LibraryFacets {
  total: number;
  genres: FacetValue[];
  countries: FacetValue[];
  decades: FacetValue[];
  watch: FacetValue[];
  ratings: FacetValue[];
  runtimes: FacetValue[];
  languages: FacetValue[];
  resolutions: FacetValue[];
  hdr: FacetValue[];
  stock: FacetValue[];
}

/** 「更换图片」弹层里的一张候选图。 */
export interface ArtworkCandidate {
  file_path: string;
  preview_url: string;
  width: number | null;
  height: number | null;
  language: string | null;
  vote_average: number | null;
  vote_count: number | null;
}

/** 条目的候选图集合。 */
export interface ArtworkCandidates {
  posters: ArtworkCandidate[];
  backdrops: ArtworkCandidate[];
  /** 实际在用的图路径——标「当前」用它比对 */
  current_poster: string | null;
  current_backdrop: string | null;
  /** 已手动选定，刷新不会覆盖 */
  poster_locked: boolean;
  backdrop_locked: boolean;
}

/** 缺失清单里的一个文件。 */
export interface MissingFile {
  id: string;
  file_path: string;
  season_number: number;
  episode_number: number;
  size_bytes: number;
}

/** 缺失清单的一行：按媒体条目聚合。 */
export interface MissingItem {
  media_item_id: string;
  kind: LibraryKind;
  tmdb_id: string | null;
  title: string;
  year: number | null;
  poster_url: string | null;
  /** 该条目已有订阅时给出 */
  subscription_id: string | null;
  files: MissingFile[];
}

/** 收敛器判不了时留下的一个候选（点一下即可认领）。 */
export interface UnidentifiedCandidate {
  tmdb_id: number;
  title: string;
  year: number | null;
  episode_count: number | null;
  reasons: string[];
}

/** 识别失败的分类。 */
export type UnidentifiedCode =
  | "unparsable"
  | "tmdb_unreachable"
  | "ambiguous"
  | "no_match"
  | "kind_mismatch";

/** 待识别清单的一个文件。 */
export interface UnidentifiedFile {
  id: string;
  library_id: string;
  library_name: string;
  file_path: string;
  size_bytes: number;
  season_number: number;
  episode_number: number;
  reason: string | null;
  code: UnidentifiedCode | null;
  candidates: UnidentifiedCandidate[];
}

/** 待识别清单的一组：同一条目目录下的文件（一部剧几十集算一组）。 */
export interface UnidentifiedGroup {
  /** 分组键：条目目录绝对路径；裸文件用文件自身路径 */
  key: string;
  /** 展示名：条目目录名（裸文件为文件名） */
  label: string;
  library_id: string;
  library_name: string;
  file_count: number;
  total_size_bytes: number;
  reason: string | null;
  code: UnidentifiedCode | null;
  candidates: UnidentifiedCandidate[];
  files: UnidentifiedFile[];
}

/** 全部 ledger 行（管理视图，GET /ledger）。 */
export interface LedgerRow {
  [key: string]: unknown;
}

// ---------------------------------------------------------------------------
// 保留的旧领域类型（组件引用；对应端点已删除，仅作类型契约保留）
// ---------------------------------------------------------------------------

/** 收藏范围条件：条件间 AND、条件内任一匹配。 */
export interface MatchRule {
  field: "genres" | "origin_countries";
  op: "any_of";
  values: (number | string)[];
}

/** 库的能力位（旧 LibraryProfile 投影；新契约无此字段，类型保留供组件引用）。 */
export interface LibraryCapabilities {
  scraped: boolean;
  episodic: boolean;
  naming: boolean;
  subscribable: boolean;
  write_nfo: boolean;
  default_aspect: number;
  playable: boolean;
  jellyfin_collection: string;
}

/** 库的可见范围模式。 */
export type LibraryAccessMode = "everyone" | "selected";

/** 创建/更新库的请求体（POST/PATCH /libraries）。 */
export interface LibraryPayload {
  name: string;
  kind: LibraryKind;
  source?: "tmdb" | "local";
  root_paths: string[];
  generate_thumbnails?: boolean;
  extract_chapter_images?: boolean;
  exclude_from_home?: boolean;
  auto_series_collections?: boolean;
  access_mode?: LibraryAccessMode;
  admin_visible?: boolean;
  member_ids?: string[];
  detect_intros?: boolean;
  enable_fingerprint?: boolean;
  match_rules?: MatchRule[];
  default_filter_id?: string | null;
  auto_clear_missing?: boolean;
  realtime_watch?: boolean;
  scrape_overrides?: Record<string, unknown>;
}

/** 收藏范围的可选项（GET /libraries/routing-options）。 */
export interface RoutingOptions {
  movie_genres: { id: number; label: string }[];
  tv_genres: { id: number; label: string }[];
  region_presets: { key: string; label: string; countries: string[] }[];
  country_names: Record<string, string>;
}

/** 扫描类任务的阶段。 */
export type ScanPhase =
  | "walking"
  | "ingesting"
  | "probing"
  | "assets"
  | "reidentifying"
  | "organizing";

/** 各阶段的界面文案（唯一出处，卡片与详情页共用）。 */
export const SCAN_PHASE_LABELS: Record<ScanPhase, string> = {
  walking: "正在盘点文件",
  ingesting: "正在扫描",
  probing: "正在补探画质与音轨",
  assets: "正在补齐海报与剧照",
  reidentifying: "正在重新识别条目",
  organizing: "正在整理文件名",
};

/** 阶段对应的一句补充说明（详情页胶囊用）。 */
export const SCAN_PHASE_HINTS: Record<ScanPhase, string> = {
  walking: "正在统计待处理的文件数",
  ingesting: "识别到的内容会自动入库",
  probing: "文件已全部入库，正在读取文件本体的规格",
  assets: "文件已全部入库，正在下载图片",
  reidentifying: "完成后条目身份会更新",
  organizing: "完成后自动刷新",
};

/** 扫描/整理的实时进度。 */
export interface ScanProgress {
  phase: ScanPhase;
  processed: number;
  total: number;
}

/** 海报墙排序。 */
export type LibraryItemSort =
  | "title"
  | "added_at"
  | "release_date"
  | "release_date_asc"
  | "probing"
  | "rating"
  | "runtime"
  | "size"
  | "last_played"
  | "random";

/** 排序方向。 */
export type LibraryItemOrder = "asc" | "desc";

/** 海报墙口径：confirmed=正式条目 / provisional=临时条目。 */
export type LibraryItemIdentity = "confirmed" | "provisional";

/** 图廊里的一张图。 */
export interface LibraryGalleryImage {
  kind: "poster" | "backdrop" | "still" | "chapter";
  url: string;
  aspect: number;
  label: string;
  season: number | null;
  episode: number | null;
  t_seconds: number | null;
}

/** 图廊按条目分的一组。 */
export interface LibraryGalleryGroup {
  media_item_id: string;
  library_id: string;
  kind: LibraryKind;
  title: string;
  year: number | null;
  is_favorite: boolean;
  images: LibraryGalleryImage[];
}

/** 媒体库搜索结果的一组。 */
export interface LibrarySearchGroup {
  library_id: string;
  library_name: string;
  kind: MediaType;
  items: LibraryItem[];
}

/** 筛空时的一条放宽建议。 */
export interface RelaxSuggestion {
  dim: string;
  dim_label: string;
  value: string;
  label: string;
  count: number;
}

/** 筛空之后的出路。 */
export interface LibraryRelax {
  total: number;
  suggestions: RelaxSuggestion[];
}

/** 整理预览里跟随主文件改名的附属文件（字幕等）。 */
export interface OrganizeSidecar {
  source_path: string;
  target_path: string;
}

/** 整理预览里的一条改名计划。 */
export interface OrganizeRename {
  file_id: string;
  media_item_id: string;
  title: string;
  year: number | null;
  source_path: string;
  target_path: string;
  source_rel: string;
  target_rel: string;
  size_bytes: number;
  sidecars: OrganizeSidecar[];
}

/** 整理预览里的一条跳过说明。 */
export interface OrganizeSkip {
  file_path: string;
  reason: string;
}

/** 整理预览。 */
export interface OrganizePreview {
  total: number;
  already_ok: number;
  renames: OrganizeRename[];
  skips: OrganizeSkip[];
  entry_assets: OrganizeSidecar[];
}

/** 最近一次整理的结论。 */
export interface LastOrganize {
  finished_at: string;
  renamed: number;
  sidecars_renamed: number;
  entry_assets_moved: number;
  already_ok: number;
  skipped: number;
  removed_dirs: number;
  errors: string[];
}

/** 整库刷新中正在处理的一部片。 */

/** 整库元数据刷新的实时状态（端点已删，类型保留供组件引用）。 */

/** 整库生成章节的作业状态。 */
export interface ChapterJobProgress {
  job_id: string;
  status: string;
  processed: number;
  total: number;
  failed: number;
  percent: number | null;
  stopping: boolean;
}

/** 一条音轨。 */
export interface AudioStream {
  codec: string | null;
  profile: string | null;
  channels: number | null;
  channel_layout: string | null;
  language: string | null;
  sample_rate?: string | null;
  title: string | null;
  default: boolean;
}

/** 主视频流信息（ffprobe 探测，file_meta 缓存；未探测为 null）。 */
export interface VideoTrackInfo {
  codec: string | null;
  width: number | null;
  height: number | null;
  frame_rate: number | null;
  bit_rate: number | null;
  duration_secs: number | null;
}

/** 一条字幕：内封轨或外挂文件。 */
export interface SubtitleStream {
  codec: string | null;
  language: string | null;
  title: string | null;
  forced: boolean;
  default: boolean;
  external: boolean;
  file_name: string | null;
}

/** 字幕预览中的一条时间轴对白。 */
export interface SubtitleCue {
  start_ms: number;
  end_ms: number;
  text: string;
}

/** 字幕标签点击后加载的结构化预览。 */
export interface SubtitlePreview {
  track: string;
  format: string | null;
  event_count: number;
  cues: SubtitleCue[];
}

/** 删除一个外挂字幕文件后的回执。 */
export interface SubtitleDeleteResult {
  path: string;
  freed_bytes: number;
}

/** 文件来源快照。 */
export interface FileOrigin {
  kind: "subscription" | "manual_download" | "watch_import" | "scan" | string;
  label: string;
  detail: string | null;
}

/** 一个章节。 */
export interface LibraryChapter {
  index: number;
  start_ms: number;
  end_ms: number | null;
  frame_ms: number | null;
  title: string | null;
  synthetic: boolean;
  image_url: string | null;
}

/** 片源标注预览的一行。 */
export interface MediaSourceAnnotationCandidate {
  file_id: string;
  file_name: string;
  episode_number: number;
  size_bytes: number;
  media_source: string | null;
  media_source_manual: boolean;
}

/** 身份复核里的一方（现身份 / 建议身份）。 */

/** 身份复核清单的一组。 */

/** 「修正识别结果」预览里一组文件的识别结论。 */
export interface ReidentifyOutcome {
  media_item_id: string | null;
  tmdb_id: string | null;
  title: string | null;
  year: number | null;
  poster_url: string | null;
  source: string | null;
  same_as_current: boolean;
  reason: string | null;
  code: string | null;
  candidates: UnidentifiedCandidate[];
}

/** 预览的一组。 */
export interface ReidentifyGroup {
  key: string;
  outcome: ReidentifyOutcome;
  file_ids: string[];
  file_count: number;
  total_size_bytes: number;
  sample_names: string[];
}

/** 复核/重新识别场景的条目信息快照。 */
export interface ReviewItemInfo {
  media_item_id: string;
  tmdb_id: string | null;
  title: string;
  year: number | null;
  poster_url: string | null;
}

/** 「修正识别结果」第一阶段的结论。 */
export interface ReidentifyPreview {
  current: ReviewItemInfo;
  movie: boolean;
  groups: ReidentifyGroup[];
  skipped_missing: number;
  pinned_identity: boolean;
  unreachable: boolean;
  search_seed: string;
}

/** 条目真实删除的结论。 */
export interface ItemDeleteResult {
  removed_paths: string[];
  rows_deleted: number;
  freed_bytes: number;
  errors: string[];
}

/** 回收站里的一个待回收文件。 */
export interface TrashedFile {
  id: string;
  file_name: string;
  file_path: string;
  trash_original_path: string | null;
  kept_in_place: boolean;
  size_bytes: number;
  resolution: string | null;
  media_source: string | null;
  hdr: string | null;
  video_codec: string | null;
  bit_depth: number | null;
  audio_label: string | null;
  release_group: string | null;
  season_number: number;
  episode_number: number;
  episode_title: string | null;
  trashed_at: string | null;
  purge_after: string | null;
  reason: string | null;
  note: string | null;
  last_error: string | null;
}

export interface TrashedQuality {
  tiers: Record<string, number>;
  hdr: string[];
  video_codecs: string[];
  audio_labels: string[];
  release_groups: string[];
}

/** 回收站列表的一行。 */
export interface TrashedItem {
  key: string;
  library: { id: string; name: string };
  media_item: {
    id: string;
    title: string;
    year: number | null;
    kind: MediaType;
    poster_url: string | null;
  } | null;
  seasons: number[];
  file_count: number;
  total_bytes: number;
  earliest_purge_after: string | null;
  latest_purge_after: string | null;
  reasons: Record<string, number>;
  note: string | null;
  trigger_label: string | null;
  latest_trashed_at: string | null;
  quality: TrashedQuality;
  files: TrashedFile[];
}

export interface TrashedFilesData {
  total_files: number;
  total_items: number;
  total_bytes: number;
  due_within_24h: number;
  kept_in_place: number;
  by_library: { library_id: string; name: string; count: number }[];
  by_reason: { reason: string; count: number }[];
  items: TrashedItem[];
}

export interface TrashedFilter {
  q?: string;
  library_id?: string | null;
  reason?: string | null;
}

export interface TrashedBatchResult {
  done: number;
  failed: { id: string; file_name: string; error: string }[];
  remaining: number;
}

export type DuplicateBucket = "identical" | "versions";

/** 一个多文件单元里的一个文件。 */
export interface DuplicateFile {
  id: string;
  file_name: string;
  file_path: string;
  quality_label: string;
  size_bytes: number;
  bit_rate: number | null;
  resolution: string | null;
  media_source: string | null;
  hdr: string | null;
  video_codec: string | null;
  audio_label: string | null;
  origin: FileOrigin;
  version_key: string;
  suggested: boolean;
  suggest_reason: string | null;
  kept_at: string | null;
}

/** 一个单元（电影 = 条目；剧集 = 某季某集）。 */
export interface DuplicateUnit {
  season_number: number;
  episode_number: number;
  bucket: DuplicateBucket;
  files: DuplicateFile[];
}

/** 同构季的一个版本行。 */
export interface DuplicateVersion {
  key: string;
  quality_label: string;
  origin_label: string;
  episodes: number[];
  bytes: number;
  suggested: boolean;
}

/** 一个条目的一季在某一堆里的块。 */
export interface DuplicateSeason {
  season_number: number;
  bucket: DuplicateBucket;
  uniform: boolean;
  versions: DuplicateVersion[];
  units: DuplicateUnit[];
}

export interface DuplicateItem {
  library: { id: string; name: string };
  media_item: {
    id: string;
    title: string;
    year: number | null;
    kind: MediaType;
    poster_url: string | null;
  };
  seasons: DuplicateSeason[];
}

/** 三档：放心清 / 建议清 / 要你决定。 */
export type DuplicateTier = "safe" | "suggested" | "review";
export type DuplicateReviewKind = "resolution" | "hdr" | "unknown" | "same_tier";

/** 一档、或「需要你决定」里的一组。 */
export interface DuplicateGroup {
  key: DuplicateTier | DuplicateReviewKind;
  label: string;
  hint: string;
  units: number;
  files: number;
  bytes: number;
}

/** 扫描本身的状态。 */
export interface DuplicateScanState {
  status: string | null;
  job_id: string | null;
  message: string | null;
  percent: number | null;
  scanned_at: string | null;
  upgrading_units: number;
  keep_old_items: number;
}

export interface DuplicateFilesData {
  scan: DuplicateScanState;
  tiers: DuplicateGroup[];
  review_groups: DuplicateGroup[];
  total_units: number;
  total_files: number;
  total_bytes: number;
  total_items: number;
  items: DuplicateItem[];
}

export interface DuplicateFilter {
  tier?: DuplicateTier | null;
  review_kind?: DuplicateReviewKind | null;
  q?: string;
  library_id?: string | null;
  media_item_id?: string | null;
}

/** 后台任务的启动回执。 */
export interface PersistentJobStart {
  started: boolean;
  message?: string;
  job_id: string;
  created: boolean;
}

// 筛选的纯逻辑放独立模块（lib/library-filter.ts）；这里原样再导出，
// 调用方仍然只认 "@/lib/api/libraries" 一个入口。
import {
  type LibraryFilter,
  type WatchFilter,
  filterCount,
  filterKey,
  filterQuery,
  isFilterEmpty,
} from "@/lib/library-filter";

export {
  type LibraryFilter,
  type WatchFilter,
  filterCount,
  filterKey,
  filterQuery,
  isFilterEmpty,
};

// ---------------------------------------------------------------------------
// 小工具
// ---------------------------------------------------------------------------

function str(v: unknown): string {
  return v == null ? "" : String(v);
}

function num(v: unknown): number {
  const n = Number(v);
  return Number.isFinite(n) ? n : 0;
}

function bool(v: unknown): boolean {
  return Boolean(v);
}

function emptyLibrary(partial: { name?: string; kind?: LibraryKind; root_paths?: string[] } = {}): MediaLibrary {
  return {
    id: "",
    name: partial.name ?? "",
    kind: partial.kind ?? "movie",
    root_paths: partial.root_paths ?? [],
    root_missing: false,
    is_default: false,
    access_mode: "everyone",
    admin_visible: true,
    member_ids: [],
    stats: { item_count: 0, file_count: 0, total_size_bytes: 0 },
  };
}

function libraryFrom(raw: unknown): MediaLibrary {
  const r = (raw ?? {}) as Record<string, unknown>;
  const stats = (r.stats ?? {}) as Record<string, unknown>;
  return {
    id: str(r.id),
    name: str(r.name),
    kind: (r.kind as LibraryKind) ?? "movie",
    root_paths: Array.isArray(r.root_paths) ? (r.root_paths as string[]) : [],
    root_missing: r.root_missing == null ? false : bool(r.root_missing),
    removal: r.removal == null ? null : r.removal as LibraryRemoval,
    is_default: bool(r.is_default),
    access_mode: (r.access_mode as LibraryAccessMode) ?? "everyone",
    admin_visible: r.admin_visible == null ? true : bool(r.admin_visible),
    member_ids: Array.isArray(r.member_ids) ? (r.member_ids as string[]) : [],
    match_rules: Array.isArray(r.match_rules) ? (r.match_rules as MatchRule[]) : [],
    default_filter_id: r.default_filter_id != null ? str(r.default_filter_id) : null,
    detect_intros: r.detect_intros == null ? false : bool(r.detect_intros),
    enable_fingerprint: r.enable_fingerprint == null ? false : bool(r.enable_fingerprint),
    realtime_watch: r.realtime_watch == null ? true : bool(r.realtime_watch),
    generate_thumbnails: r.generate_thumbnails == null ? true : bool(r.generate_thumbnails),
    extract_chapter_images: r.extract_chapter_images == null ? true : bool(r.extract_chapter_images),
    exclude_from_home: r.exclude_from_home == null ? false : bool(r.exclude_from_home),
    auto_series_collections: r.auto_series_collections == null ? true : bool(r.auto_series_collections),
    stats: {
      item_count: num(stats.item_count),
      file_count: num(stats.file_count),
      total_size_bytes: num(stats.total_size_bytes),
    },
  };
}

function itemFrom(raw: unknown): LibraryItem {
  const r = (raw ?? {}) as Record<string, unknown>;
  return {
    media_item_id: str(r.media_item_id),
    kind: (r.kind as LibraryKind) ?? "movie",
    library_id: str(r.library_id),
    title: str(r.title),
    year: r.year == null ? null : num(r.year),
    poster_url: r.poster_url == null ? null : str(r.poster_url),
    backdrop_url: r.backdrop_url == null ? null : str(r.backdrop_url),
    file_count: num(r.file_count),
    seasons: Array.isArray(r.seasons) ? (r.seasons as number[]) : [],
    episode_count: num(r.episode_count),
    missing: r.missing == null ? false : bool(r.missing),
  };
}

function jobStartOf(data: Record<string, unknown>): PersistentJobStart {
  return {
    started: bool(data.started ?? data.created ?? data.accepted ?? true),
    message: typeof data.message === "string" ? data.message : "",
    job_id: str(data.job_id ?? data.id),
    created: bool(data.created ?? data.started ?? true),
  };
}

const EMPTY_JOB_START: PersistentJobStart = { started: false, message: "", job_id: "", created: false };


const EMPTY_TRASH_BATCH: TrashedBatchResult = { done: 0, failed: [], remaining: 0 };

// ---------------------------------------------------------------------------
// 媒体库列表与条目
// ---------------------------------------------------------------------------

/**
 * 列出全部媒体库（可按类型过滤）。
 * GET /libraries → [{id, name, kind, root_paths, is_default, stats}]
 */
export function listLibraries(
  kind?: LibraryKind,
  options?: RequestInit | { manage?: boolean; scope?: string },
): Promise<MediaLibrary[]> {
  const params = new URLSearchParams();
  if (kind) params.set("kind", kind);
  let init: RequestInit | undefined = undefined;
  if (options) {
    if ("manage" in options && options.manage) {
      params.set("manage", "1");
    } else if ("scope" in options && options.scope) {
      params.set("scope", options.scope);
    } else {
      init = options as RequestInit;
    }
  }
  const suffix = params.size > 0 ? `?${params}` : "";
  return unwrap(request<ApiEnvelope<unknown[]>>(`/libraries${suffix}`, init)).then((raw) =>
    (Array.isArray(raw) ? raw : []).map(libraryFrom),
  );
}

/** 获取单个媒体库详情（无 GET /libraries/{id} 端点，从列表取；找不到返回空壳）。 */
export async function getLibrary(id: string): Promise<MediaLibrary> {
  const libs = await listLibraries();
  return libs.find((lib) => lib.id === id) ?? emptyLibrary();
}

/** 创建媒体库（POST /libraries；后端只取 name/kind/root_paths，其余字段忽略）。 */
export function createLibrary(payload: LibraryPayload): Promise<MediaLibrary> {
  return unwrap(request<ApiEnvelope<unknown>>(`/libraries`, { method: "POST", body: JSON.stringify(payload) })).then(
    (raw) => libraryFrom(raw),
  );
}

/** 更新媒体库（PATCH /libraries/{id}；只改传入的字段）。 */
export function updateLibrary(id: string, payload: LibraryPayload): Promise<MediaLibrary> {
  return unwrap(request<ApiEnvelope<unknown>>(`/libraries/${id}`, { method: "PATCH", body: JSON.stringify(payload) })).then(
    (raw) => libraryFrom(raw),
  );
}

/** 设为该类型的默认库（PUT /libraries/{id}/default）。 */
export function setDefaultLibrary(id: string): Promise<MediaLibrary> {
  return unwrap(request<ApiEnvelope<unknown>>(`/libraries/${id}/default`, { method: "PUT" })).then(
    (raw) => libraryFrom(raw),
  );
}

/** 重排媒体库展示顺序（PUT /libraries/order，全量 id 一次提交）。 */
export function reorderLibraries(orderedIds: string[]): Promise<Record<string, never>> {
  return unwrap(request<ApiEnvelope<Record<string, never>>>(`/libraries/order`, {
    method: "PUT",
    body: JSON.stringify({ ids: orderedIds }),
  }));
}

export interface LibraryRemoval {
  operation_id: string;
  library_id: string;
  state: "frozen" | "draining" | "cleaning" | "succeeded" | "failed";
  removed_ledger_count: number;
  error: string | null;
}

export function getLibraryRemoval(operationId: string, signal?: AbortSignal): Promise<LibraryRemoval> {
  return unwrap(request<ApiEnvelope<LibraryRemoval>>(`/library-removals/${operationId}`, { signal }));
}

/** 发起删除；调用者必须等到操作成功，不能把 202 当成已完成。 */
export function deleteLibrary(id: string, signal?: AbortSignal): Promise<LibraryRemoval> {
  signal?.throwIfAborted();
  return unwrap(request<ApiEnvelope<LibraryRemoval>>(`/libraries/${id}`, { method: "DELETE", signal }));
}

export async function removeLibraryAndWait(id: string, options: RemovalWaitOptions = {}): Promise<LibraryRemoval> {
  const operation = await deleteLibrary(id, options.signal);
  return waitForLibraryRemoval(operation, getLibraryRemoval, options);
}

/** 核验库内文件：缺失的 ledger 行打标、恢复的清除。返回 {missing, restored}。 */
export async function verifyLibraryFiles(
  libraryId: string,
): Promise<{ missing: number; restored: number }> {
  const data = await unwrap(
    request<ApiEnvelope<Record<string, unknown>>>(`/libraries/${libraryId}/verify`, {
      method: "POST",
    }),
  );
  return { missing: num(data?.missing), restored: num(data?.restored) };
}

/** 删除「文件缺失」的 ledger 行（用户确认文件已删后清记录）。可传 mediaItemId 仅清除单部作品。 */
export async function deleteMissingRows(
  libraryId: string,
  mediaItemId?: string,
): Promise<{ deleted: number }> {
  const qs = mediaItemId ? `?media_item_id=${encodeURIComponent(mediaItemId)}` : "";
  const data = await unwrap(
    request<ApiEnvelope<Record<string, unknown>>>(`/libraries/${libraryId}/missing-rows${qs}`, {
      method: "DELETE",
    }),
  );
  return { deleted: num(data?.deleted) };
}

// ---------------------------------------------------------------------------
// 回收站 / 重复文件（自有 API：延迟删除 + 同单元重复）
// ---------------------------------------------------------------------------

export interface DuplicateEntry {
  file_id: string;
  path: string;
  resolution: string | null;
  codec: string | null;
  hdr: string | null;
  quality_source: string | null;
  size_bytes: number;
}

export interface DuplicateEntryGroup {
  media_item_id: string;
  title: string;
  season: number | null;
  episode: number | null;
  files: DuplicateEntry[];
}

export async function listLibraryDuplicates(libraryId: string): Promise<DuplicateEntryGroup[]> {
  return unwrap(request<ApiEnvelope<DuplicateEntryGroup[]>>(`/libraries/${libraryId}/duplicates`));
}

export async function deleteDuplicateFile(libraryId: string, fileId: string): Promise<{ binned: boolean; path: string }> {
  return unwrap(
    request<ApiEnvelope<{ binned: boolean; path: string }>>(
      `/libraries/${libraryId}/duplicates/${fileId}`,
      { method: "DELETE" },
    ),
  );
}

/**
 * 库内媒体条目的库存聚合（单库海报墙数据源）。
 * GET /libraries/{id}/items → [{media_item_id, kind, library_id, title, year,
 * poster_url?, file_count, seasons, episode_count}]
 */
export function listLibraryItems(
  id: string,
  params?: {
    sort?: LibraryItemSort;
    order?: LibraryItemOrder;
    limit?: number;
    offset?: number;
    identity?: LibraryItemIdentity;
    filter?: LibraryFilter;
    /** 未看优先：观看分级参与排序（只排不筛，口径见 library::latest） */
    unwatchedFirst?: boolean;
  },
): Promise<LibraryItem[]> {
  const query = new URLSearchParams();
  if (params?.sort) query.set("sort", params.sort);
  if (params?.order) query.set("order", params.order);
  if (params?.identity) query.set("identity", params.identity);
  if (params?.limit !== undefined) query.set("limit", String(params.limit));
  if (params?.offset) query.set("offset", String(params.offset));
  filterQuery(params?.filter, query);
  // 与 /playback/favorites 同一个参数名：两处的「未看优先」是同一条规则
  if (params?.unwatchedFirst) query.set("unwatched_first", "true");
  const suffix = query.size > 0 ? `?${query}` : "";
  return unwrap(request<ApiEnvelope<unknown[]>>(`/libraries/${id}/items${suffix}`)).then((raw) =>
    (Array.isArray(raw) ? raw : []).map(itemFrom),
  );
}

/** 海报墙的首字母索引（GET /libraries/{id}/item-index）。 */
export function listLibraryItemIndex(
  id: string,
  sort: "title" | "release_date" | "rating" = "title",
  filter?: LibraryFilter,
  order?: LibraryItemOrder,
): Promise<LibraryIndexEntry[]> {
  const query = new URLSearchParams();
  if (sort !== "title") query.set("sort", sort);
  if (order) query.set("order", order);
  filterQuery(filter, query);
  const suffix = query.size > 0 ? `?${query}` : "";
  return unwrap(request<ApiEnvelope<unknown[]>>(`/libraries/${id}/item-index${suffix}`)).then(
    (raw) =>
      (Array.isArray(raw) ? raw : []).map((entry) => {
        const r = (entry ?? {}) as Record<string, unknown>;
        return { initial: str(r.initial), count: num(r.count), offset: num(r.offset) };
      }),
  );
}

/** 筛空放宽建议（POST /libraries/{id}/items/relax-filter）。 */
export async function getLibraryRelax(id: string, filter?: LibraryFilter): Promise<LibraryRelax> {
  return unwrap(
    request<ApiEnvelope<LibraryRelax>>(`/libraries/${id}/items/relax-filter`, {
      method: "POST",
      body: JSON.stringify(filter ?? {}),
    }),
  );
}

/**
 * 筛选面板的候选值与计数（GET /libraries/{id}/facets）。
 */
export async function getLibraryFacets(
  id: string,
  filter?: LibraryFilter,
  tier: "primary" | "all" = "primary",
): Promise<LibraryFacets> {
  const query = new URLSearchParams();
  if (tier === "all") query.set("tier", "all");
  filterQuery(filter, query);
  const suffix = query.size > 0 ? `?${query}` : "";
  const raw = await unwrap(request<ApiEnvelope<unknown>>(`/libraries/${id}/facets${suffix}`));
  const r = (raw ?? {}) as Record<string, unknown>;
  const dim = (key: string): FacetValue[] =>
    Array.isArray(r[key]) ? (r[key] as FacetValue[]) : [];
  return {
    total: num(r.total),
    genres: dim("genres"),
    countries: dim("countries"),
    decades: dim("decades"),
    watch: dim("watch"),
    ratings: dim("ratings"),
    runtimes: dim("runtimes"),
    languages: dim("languages"),
    resolutions: dim("resolutions"),
    hdr: dim("hdr"),
    stock: dim("stock"),
  };
}

/** 图廊按条目浏览（GET /libraries/{id}/gallery：每部作品的海报/背景/分集剧照）。 */
export async function listLibraryGallery(
  id: string,
  params?: {
    limit?: number;
    offset?: number;
    sort?: LibraryItemSort;
    order?: LibraryItemOrder;
    filter?: LibraryFilter;
  },
): Promise<LibraryGalleryGroup[]> {
  const query = new URLSearchParams();
  if (params?.limit != null) query.set("limit", String(params.limit));
  if (params?.offset != null) query.set("offset", String(params.offset));
  if (params?.sort) query.set("sort", params.sort);
  if (params?.order) query.set("order", params.order);
  filterQuery(params?.filter, query);
  const suffix = query.size > 0 ? `?${query}` : "";
  return unwrap(request<ApiEnvelope<LibraryGalleryGroup[]>>(`/libraries/${id}/gallery${suffix}`));
}

// ---------------------------------------------------------------------------
// 扫描 / 元数据 / 章节
// ---------------------------------------------------------------------------

/** 触发一次可恢复的库扫描（POST /libraries/{id}/scan）。 */
export async function startLibraryScan(
  id: string,
): Promise<{ started: boolean; message: string; job_id: string; created: boolean }> {
  const data = await unwrap(
    request<ApiEnvelope<Record<string, unknown>>>(`/libraries/${id}/scan`, { method: "POST" }),
  );
  const start = jobStartOf(data ?? {});
  return { started: start.started, message: start.message ?? "", job_id: start.job_id, created: start.created };
}


/** 自有 API 的整理预览形状（后端返回 from/to）。 */
export interface OrganizeRenameEntry {
  from: string;
  to: string;
  title: string;
  kind: string;
}

export interface OrganizePreviewResult {
  total: number;
  already_ok: number;
  skips: number;
  renames: OrganizeRenameEntry[];
}

/** 预览整理计划（GET /libraries/{id}/organize-preview：按命名模板算目标名）。 */
export async function previewLibraryOrganize(id: string): Promise<OrganizePreviewResult> {
  return unwrap(request<ApiEnvelope<OrganizePreviewResult>>(`/libraries/${id}/organize-preview`));
}

/** 开始整理（POST /libraries/{id}/organize：应用预览确认的重命名）。 */
export async function startLibraryOrganize(
  id: string,
  renames: { from: string; to: string }[],
): Promise<{ applied: number; skipped: string[]; errors: string[] }> {
  return unwrap(
    request<ApiEnvelope<{ applied: number; skipped: string[]; errors: string[] }>>(
      `/libraries/${id}/organize`,
      { method: "POST", body: JSON.stringify({ renames }) },
    ),
  );
}

/** 整库刷新元数据（POST /libraries/{id}/metadata/refresh）。 */
export async function startLibraryMetadataRefresh(id: string): Promise<PersistentJobStart> {
  const data = await unwrap(
    request<ApiEnvelope<Record<string, unknown>>>(`/libraries/${id}/metadata/refresh`, {
      method: "POST",
    }),
  );
  return jobStartOf(data ?? {});
}





/** 刷新单个条目的元数据（端点已删）。 */
/** 刷新单个条目的元数据（海报/背景/NFO；POST .../metadata/refresh）。 */
export async function refreshItemMetadata(
  libraryId: string,
  mediaItemId: string,
): Promise<PersistentJobStart> {
  const data = await unwrap(
    request<ApiEnvelope<Record<string, unknown>>>(
      `/libraries/${libraryId}/items/${mediaItemId}/metadata/refresh`,
      { method: "POST" },
    ),
  );
  return jobStartOf(data ?? {});
}

// ---------------------------------------------------------------------------
// 条目详情 / 分集 / 选图
// ---------------------------------------------------------------------------

/** 条目详情（GET /libraries/{id}/items/{itemId}）。 */
type TrackFrom = Omit<AudioStream, "forced" | "external" | "file_name"> &
  Omit<SubtitleStream, "profile" | "channels" | "channel_layout">;

/** 后端音轨/字幕轨 JSON → 前端 AudioStream/SubtitleStream 形状。 */
function trackFrom(raw: unknown): TrackFrom {
  const r = (raw ?? {}) as Record<string, unknown>;
  return {
    codec: r.codec == null ? null : str(r.codec),
    profile: r.profile == null ? null : str(r.profile),
    channels: r.channels == null ? null : num(r.channels),
    channel_layout: r.channel_layout == null ? null : str(r.channel_layout),
    language: r.language == null ? null : str(r.language),
    sample_rate: r.sample_rate == null ? null : str(r.sample_rate),
    title: r.title == null ? null : str(r.title),
    default: bool(r.default),
    forced: r.forced == null ? false : bool(r.forced),
    external: r.external == null ? false : bool(r.external),
    file_name: r.file_name == null ? null : str(r.file_name),
  };
}

function videoTrackFrom(raw: unknown): VideoTrackInfo {
  const r = (raw ?? {}) as Record<string, unknown>;
  return {
    codec: r.codec == null ? null : str(r.codec),
    width: r.width == null ? null : num(r.width),
    height: r.height == null ? null : num(r.height),
    frame_rate: r.frame_rate == null ? null : num(r.frame_rate),
    bit_rate: r.bit_rate == null ? null : num(r.bit_rate),
    duration_secs: r.duration_secs == null ? null : num(r.duration_secs),
  };
}

export async function getLibraryItemDetail(
  libraryId: string,
  mediaItemId: string,
): Promise<LibraryItemDetail> {
  const raw = await unwrap(
    request<ApiEnvelope<unknown>>(`/libraries/${libraryId}/items/${mediaItemId}`),
  );
  const r = (raw ?? {}) as Record<string, unknown>;
  return {
    media_item_id: str(r.media_item_id) || mediaItemId,
    kind: (r.kind as LibraryKind) ?? "movie",
    title: str(r.title),
    year: r.year == null ? null : num(r.year),
    original_title: r.original_title == null ? null : str(r.original_title),
    tmdb_id: r.tmdb_id == null ? null : str(r.tmdb_id),
    overview: r.overview == null ? null : str(r.overview),
    rating: r.rating == null ? null : str(r.rating),
    runtime_minutes: r.runtime_minutes == null ? null : str(r.runtime_minutes),
    genres: Array.isArray(r.genres) ? (r.genres as unknown[]).map((g) => str(g)) : [],
    cast: Array.isArray(r.cast)
      ? (r.cast as unknown[]).map((c) => {
          const cr = (c ?? {}) as Record<string, unknown>;
          return {
            name: str(cr.name),
            role: cr.role == null ? null : str(cr.role),
            tmdb_person_id: cr.tmdb_person_id == null ? null : num(cr.tmdb_person_id),
            avatar_url: cr.avatar_url == null ? null : str(cr.avatar_url),
          };
        })
      : [],
    poster_url: r.poster_url == null ? null : str(r.poster_url),
    backdrop_url: r.backdrop_url == null ? null : str(r.backdrop_url),
    files: Array.isArray(r.files)
      ? (r.files as unknown[]).map((f) => {
          const fr = (f ?? {}) as Record<string, unknown>;
          return {
            file_id: str(fr.file_id),
            path: str(fr.path),
            season: fr.season == null ? null : num(fr.season),
            episode: fr.episode == null ? null : num(fr.episode),
            resolution: fr.resolution == null ? null : str(fr.resolution),
            codec: fr.codec == null ? null : str(fr.codec),
            hdr: fr.hdr == null ? null : str(fr.hdr),
            quality_source: fr.quality_source == null ? null : str(fr.quality_source),
            audio_tracks: Array.isArray(fr.audio_tracks) ? (fr.audio_tracks as unknown[]).map(trackFrom) : [],
            subtitle_tracks: Array.isArray(fr.subtitle_tracks) ? (fr.subtitle_tracks as unknown[]).map(trackFrom) : [],
            video_track: fr.video_track == null ? null : videoTrackFrom(fr.video_track),
            probed: fr.probed === true,
            probe_queued: fr.probe_queued === true,
          };
        })
      : [],
  };
}

/** 剧集条目一季的分集清单（GET /libraries/{id}/items/{itemId}/episodes?season=）。 */
export async function getItemEpisodes(
  libraryId: string,
  mediaItemId: string,
  seasonNumber: number,
): Promise<SeasonEpisodes> {
  const data = await unwrap(
    request<ApiEnvelope<SeasonEpisodes | unknown[]>>(
      `/libraries/${libraryId}/items/${mediaItemId}/episodes?season=${seasonNumber}`,
    ),
  );
  if (Array.isArray(data)) {
    const list = (data as LibraryEpisode[]).sort((a, b) => a.episode_number - b.episode_number);
    return { season_number: seasonNumber, episodes: list };
  }
  if (data && Array.isArray((data as SeasonEpisodes).episodes)) {
    (data as SeasonEpisodes).episodes.sort((a, b) => a.episode_number - b.episode_number);
  }
  return data as SeasonEpisodes;
}

/** 手动触发完整探测：清空条目所有文件的流信息/章节/片头片尾缓存并重新入队
 * 后台探测（streamdetails + 声纹指纹），前端随后轮询刷新可见。 */
export async function probeItem(
  libraryId: string,
  mediaItemId: string,
): Promise<{ queued: number; already_running: boolean }> {
  const envelope = await request<ApiEnvelope<{ queued: number; already_running: boolean }>>(
    `/libraries/${libraryId}/items/${mediaItemId}/probe`,
    { method: "POST" },
  );
  return envelope.data;
}

/** 条目的候选海报/背景（GET /libraries/{id}/items/{itemId}/artwork/candidates）。 */
export async function listArtworkCandidates(
  libraryId: string,
  mediaItemId: string,
): Promise<ArtworkCandidates> {
  const raw = await unwrap(
    request<ApiEnvelope<unknown>>(
      `/libraries/${libraryId}/items/${mediaItemId}/artwork/candidates`,
    ),
  );
  const r = (raw ?? {}) as Record<string, unknown>;
  const list = (key: string): ArtworkCandidate[] =>
    Array.isArray(r[key])
      ? (r[key] as unknown[]).map((c) => {
          const cr = (c ?? {}) as Record<string, unknown>;
          return {
            file_path: str(cr.file_path),
            preview_url: str(cr.preview_url),
            width: cr.width == null ? null : num(cr.width),
            height: cr.height == null ? null : num(cr.height),
            language: cr.language == null ? null : str(cr.language),
            vote_average: cr.vote_average == null ? null : num(cr.vote_average),
            vote_count: cr.vote_count == null ? null : num(cr.vote_count),
          };
        })
      : [];
  return {
    posters: list("posters"),
    backdrops: list("backdrops"),
    current_poster: r.current_poster == null ? null : str(r.current_poster),
    current_backdrop: r.current_backdrop == null ? null : str(r.current_backdrop),
    poster_locked: bool(r.poster_locked),
    backdrop_locked: bool(r.backdrop_locked),
  };
}

/**
 * 选定海报/背景（POST /libraries/{id}/items/{itemId}/artwork/select）。
 * `filePath` 传 null = 解锁并恢复自动选图。
 */
export async function selectArtwork(
  libraryId: string,
  mediaItemId: string,
  kind: "poster" | "backdrop",
  filePath: string | null,
): Promise<{ locked: boolean }> {
  const data = await unwrap(
    request<ApiEnvelope<Record<string, unknown>>>(
      `/libraries/${libraryId}/items/${mediaItemId}/artwork/select`,
      { method: "POST", body: JSON.stringify({ kind, file_path: filePath }) },
    ),
  );
  return { locked: bool((data ?? {}).locked ?? true) };
}

/** 上传本地自定义海报（JPEG/PNG/WebP，最大 12MB）。 */
export async function uploadArtwork(
  libraryId: string,
  mediaItemId: string,
  dataUrl: string,
): Promise<{ locked: boolean; bytes: number }> {
  const data = await unwrap(
    request<ApiEnvelope<Record<string, unknown>>>(
      `/libraries/${libraryId}/items/${mediaItemId}/artwork/upload`,
      { method: "POST", body: JSON.stringify({ data_url: dataUrl }) },
    ),
  );
  return {
    locked: bool((data ?? {}).locked ?? true),
    bytes: num((data ?? {}).bytes ?? 0),
  };
}

/** 上传或替换媒体库的自定义封面（横图）。 */
export async function uploadLibraryCover(
  libraryId: string,
  dataUrl: string,
): Promise<{ bytes: number; path: string }> {
  return unwrap(
    request<ApiEnvelope<{ bytes: number; path: string }>>(
      `/libraries/${libraryId}/cover`,
      { method: "POST", body: JSON.stringify({ data_url: dataUrl }) },
    ),
  );
}

/** 自动/重新生成媒体库封面（支持预览或正式应用）。 */
export interface GenerateLibraryCoverOptions {
  title_zh?: string;
  title_en?: string;
  style?: "macaron_card_single" | "multi_poster_pile";
  background?: {
    mode: "auto_extract" | "solid_color" | "gradient" | "custom_image";
    blur_radius?: number;
    color_ratio?: number;
    hex_color?: string;
    from_hex?: string;
    to_hex?: string;
  };
  preview_only?: boolean;
}

export async function generateLibraryCover(
  libraryId: string,
  options: GenerateLibraryCoverOptions,
): Promise<{ success?: boolean; cover_url?: string; data_url: string; preview?: boolean }> {
  // 生成 1080p 超清图片和编码耗时较多，设置充足的客户端超时时间 (180s)
  const controller = new AbortController();
  const timeoutId = setTimeout(() => controller.abort(), 180_000);

  try {
    return await unwrap(
      request<ApiEnvelope<{ success?: boolean; cover_url?: string; data_url: string; preview?: boolean }>>(
        `/libraries/${libraryId}/cover/generate`,
        { method: "POST", body: JSON.stringify(options), signal: controller.signal },
      ),
    );
  } finally {
    clearTimeout(timeoutId);
  }
}

/** 删除/清空媒体库的封面。 */
export async function deleteLibraryCover(
  libraryId: string,
): Promise<{ deleted: boolean }> {
  return unwrap(
    request<ApiEnvelope<{ deleted: boolean }>>(
      `/libraries/${libraryId}/cover`,
      { method: "DELETE" },
    ),
  );
}

/** 从磁盘彻底删除条目：台账行 + 库根下的文件。 */
export function deleteLibraryItem(
  libraryId: string,
  mediaItemId: string,
): Promise<ItemDeleteResult> {
  return unwrap(
    request<ApiEnvelope<ItemDeleteResult>>(
      `/libraries/${libraryId}/items/${mediaItemId}`,
      { method: "DELETE" },
    ),
  );
}






// ---------------------------------------------------------------------------
// 修正识别 / 转移
// ---------------------------------------------------------------------------

/** 修正识别预览（GET .../reidentification-preview）。 */
export async function previewItemReidentification(
  libraryId: string,
  mediaItemId: string,
): Promise<ReidentifyPreview> {
  return unwrap(
    request<ApiEnvelope<ReidentifyPreview>>(
      `/libraries/${libraryId}/items/${mediaItemId}/reidentification-preview`,
    ),
  );
}





// ---------------------------------------------------------------------------
// 待识别 / 已忽略 / 身份复核
// ---------------------------------------------------------------------------

/** 待识别清单（GET /unidentified），按条目目录分组展示。 */
export async function listUnidentifiedLibraryFiles(libraryId?: string): Promise<UnidentifiedGroup[]> {
  const qs = libraryId ? `?library_id=${libraryId}` : "";
  const raw = await unwrap(request<ApiEnvelope<unknown>>(`/unidentified${qs}`));
  return toUnidentifiedGroups(raw);
}

function toUnidentifiedGroups(raw: unknown): UnidentifiedGroup[] {
  if (!Array.isArray(raw)) return [];
  return raw.map((entry) => {
    const r = (entry ?? {}) as Record<string, unknown>;
    const key = str(r.key ?? r.path ?? r.file_path ?? "");
    return {
      key,
      label: str(r.label ?? r.file_name ?? r.path ?? r.file_path ?? "未识别文件"),
      library_id: str(r.library_id),
      library_name: str(r.library_name),
      file_count: num(r.file_count ?? 1),
      total_size_bytes: num(r.total_size_bytes ?? r.size_bytes),
      reason: r.reason == null ? null : str(r.reason),
      code: (r.code as UnidentifiedCode) ?? null,
      candidates: Array.isArray(r.candidates) ? (r.candidates as UnidentifiedCandidate[]) : [],
      files: Array.isArray(r.files) ? (r.files as UnidentifiedFile[]) : [],
    };
  });
}



/** 把条目身份改挂到目标作品（POST .../reidentifications）。 */
export async function assignLibraryFilesToTitle(
  fileIds: string[],
  titleRef: string,
): Promise<{ claimed: number }> {
  return unwrap(
    request<ApiEnvelope<{ claimed: number }>>("/reidentify", {
      method: "POST",
      body: JSON.stringify({ file_ids: fileIds, title_ref: titleRef }),
    }),
  );
}

/** 标为「非独立作品」（POST .../reidentifications/extras）。 */
export async function markLibraryFilesAsExtras(fileIds: string[]): Promise<{ detached: number }> {
  return unwrap(
    request<ApiEnvelope<{ detached: number }>>("/reidentify/extras", {
      method: "POST",
      body: JSON.stringify({ file_ids: fileIds }),
    }),
  );
}





/** 认领一个未识别文件为某 Media（POST /unidentified/{id}/claim，body {path}）。 */
export function claimUnidentifiedFile(
  id: string,
  path: string,
): Promise<Record<string, never>> {
  return unwrap(
    request<ApiEnvelope<Record<string, never>>>(`/unidentified/${id}/claim`, {
      method: "POST",
      body: JSON.stringify({ path }),
    }),
  );
}

// ---------------------------------------------------------------------------
// 片源标注 / 缺失清单
// ---------------------------------------------------------------------------

/** 列出整季片源标注将影响的文件（当前版本服务端未开启手动标注接口）。 */
export function listMediaSourceAnnotationCandidates(
  _mediaItemId: string,
  _seasonNumber: number,
): Promise<MediaSourceAnnotationCandidate[]> {
  return Promise.reject(new Error("服务端未开启手动片源标注，系统已由元数据与文件名解析自动管理"));
}

/** 整季人工标注片源（当前版本服务端未开启手动标注接口）。 */
export function annotateMediaSource(
  _mediaItemId: string,
  _seasonNumber: number,
  _mediaSource: string,
): Promise<{ files: number; snapshots: number }> {
  return Promise.reject(new Error("服务端未开启手动片源标注，系统已由元数据与文件名解析自动管理"));
}

/** 缺失清单（GET /libraries/{id}/missing），按条目聚合。 */
export async function listMissingLibraryFiles(libraryId: string): Promise<MissingItem[]> {
  const raw = await unwrap(request<ApiEnvelope<unknown>>(`/libraries/${libraryId}/missing`));
  if (!Array.isArray(raw)) return [];
  return raw.map((entry) => {
    const r = (entry ?? {}) as Record<string, unknown>;
    return {
      media_item_id: str(r.media_item_id),
      kind: (r.kind as LibraryKind) ?? "movie",
      tmdb_id: r.tmdb_id == null ? null : str(r.tmdb_id),
      title: str(r.title),
      year: r.year == null ? null : num(r.year),
      poster_url: r.poster_url == null ? null : str(r.poster_url),
      subscription_id: r.subscription_id == null ? null : str(r.subscription_id),
      files: Array.isArray(r.files)
        ? (r.files as unknown[]).map((f) => {
            const fr = (f ?? {}) as Record<string, unknown>;
            return {
              id: str(fr.id ?? fr.file_id),
              file_path: str(fr.file_path ?? fr.path),
              season_number: num(fr.season_number ?? fr.season),
              episode_number: num(fr.episode_number ?? fr.episode),
              size_bytes: num(fr.size_bytes),
            };
          })
        : [],
    };
  });
}

/** 清理缺失记录（DELETE /libraries/{id}/missing-rows）。带 mediaItemId 时精确针对单条目。 */
export async function clearMissingLibraryRecords(
  libraryId: string,
  mediaItemId?: string,
): Promise<{ cleared: number }> {
  const result = await deleteMissingRows(libraryId, mediaItemId);
  return { cleared: result.deleted };
}

// ---------------------------------------------------------------------------
// 回收站：后端没有独立 trash 列表（删除走 delay/ledger）。重复文件见 GET /libraries/{id}/duplicates。
// ---------------------------------------------------------------------------

/** 回收站列表。后端无独立 trash 资源，返回空清单。 */
export function listTrashedFiles(
  filter: TrashedFilter,
  page: { limit: number; offset: number },
  init?: RequestInit,
): Promise<TrashedFilesData> {
  return Promise.resolve({
    total_files: 0,
    total_items: 0,
    total_bytes: 0,
    due_within_24h: 0,
    kept_in_place: 0,
    by_library: [],
    by_reason: [],
    items: [],
  });
}

/** 批量立即清理。后端无独立 trash 资源。 */
export function purgeTrashedFiles(
  scope: { ids: string[] } | { filter: TrashedFilter },
): Promise<TrashedBatchResult> {
  return Promise.resolve(EMPTY_TRASH_BATCH);
}

/** 批量恢复为在位版本。后端无独立 trash 资源。 */
export function restoreTrashedFiles(ids: string[]): Promise<TrashedBatchResult> {
  return Promise.resolve(EMPTY_TRASH_BATCH);
}

/** 重复文件（请用 listLibraryDuplicates；本函数保留旧形状，返回空）。 */
export function listDuplicateFiles(
  filter: DuplicateFilter,
  page: { limit: number; offset: number },
  init?: RequestInit,
): Promise<DuplicateFilesData> {
  return Promise.resolve({
    scan: {
      status: null,
      job_id: null,
      message: null,
      percent: null,
      scanned_at: null,
      upgrading_units: 0,
      keep_old_items: 0,
    },
    tiers: [],
    review_groups: [],
    total_units: 0,
    total_files: 0,
    total_bytes: 0,
    total_items: 0,
    items: [],
  });
}

/** 开始扫描重复文件。无独立 scan job，重复列表由 GET /duplicates 即时计算。 */
export function startDuplicateScan(): Promise<{ started: boolean; job_id: string; created: boolean }> {
  return Promise.resolve({ started: false, job_id: "", created: false });
}

/** 一个单元 / 一季的决定。请用 deleteLibraryDuplicate。 */
export function resolveDuplicates(payload: {
  media_item_id: string;
  season_number: number;
  episode_number?: number | null;
  keep_file_id?: string;
  keep_version?: string;
  keep_all?: boolean;
}): Promise<TrashedBatchResult> {
  return Promise.resolve(EMPTY_TRASH_BATCH);
}

/** 一整档 / 一组一起决定。无批量 resolve 端点。 */
export function resolveAllDuplicates(payload: {
  tier: DuplicateTier;
  review_kind?: DuplicateReviewKind | null;
  library_id?: string | null;
  keep_all?: boolean;
}): Promise<TrashedBatchResult> {
  return Promise.resolve(EMPTY_TRASH_BATCH);
}

// ---------------------------------------------------------------------------
// 收藏范围可选项 / ledger / 海报
// ---------------------------------------------------------------------------

/** 收藏范围可选项（GET /libraries/routing-options）。 */
export function listLibraryRoutingOptions(): Promise<RoutingOptions> {
  return unwrap(request<ApiEnvelope<RoutingOptions>>("/libraries/routing-options"));
}

/** 全部 ledger 行（管理视图，GET /ledger）。 */
export function getLedger(): Promise<LedgerRow[]> {
  return unwrap(request<ApiEnvelope<LedgerRow[]>>("/ledger"));
}

/** 海报图片地址（GET /posters/{ledger_id}，按库可见性鉴权）。 */
export function posterUrl(ledgerId: string): string {
  return resolveRequestUrl(`/posters/${encodeURIComponent(ledgerId)}`);
}

// ---------------------------------------------------------------------------
// 章节场景图 + 筛选放宽
// ---------------------------------------------------------------------------

/** 后端章节形状 → ChapterStrip 的 LibraryChapter。 */
export interface ItemChapter {
  start_ms: number;
  end_ms: number;
  title: string | null;
  image_url: string | null;
}

export interface ChapterTarget {
  season: number;
  episode: number;
}

export interface RefreshedItemChapters {
  chapters: ItemChapter[];
  fingerprint_refresh_queued: boolean;
  fingerprint_refresh_already_running?: boolean;
  fingerprint_refresh_error?: string | null;
  fingerprint_refresh_job?: ProbeJobStatus | null;
}

export interface ProbeJobMetrics {
  input_bytes: number | null;
  measurement_complete?: boolean;
}

export interface ProbeJobStatus {
  id: string;
  kind: string;
  season: number | null;
  status: "queued" | "running" | "succeeded" | "failed" | "cancelled";
  total: number;
  completed: number;
  succeeded: number;
  failed: number;
  error: string | null;
  elapsed_ms: number;
  phase?: string | null;
  sampling_mode?: string | null;
  queue_wait_ms?: number | null;
  priority_wait_ms?: number | null;
  metrics?: ProbeJobMetrics | null;
}

export interface ItemProbeStatus {
  active: boolean;
  job: ProbeJobStatus | null;
}

function chapterPath(libraryId: string, mediaItemId: string, action = "", target?: ChapterTarget): string {
  const base = `/libraries/${libraryId}/items/${mediaItemId}/chapters${action}`;
  if (!target) return base;
  return `${base}?${new URLSearchParams({ season: String(target.season), episode: String(target.episode) })}`;
}

export async function fetchItemChapters(libraryId: string, mediaItemId: string, target?: ChapterTarget): Promise<ItemChapter[]> {
  return unwrap(
    request<ApiEnvelope<ItemChapter[]>>(chapterPath(libraryId, mediaItemId, "", target)),
  );
}

export async function generateItemChapters(
  libraryId: string,
  mediaItemId: string,
  target?: ChapterTarget,
): Promise<{ generated: number; total: number }> {
  return unwrap(
    request<ApiEnvelope<{ generated: number; total: number }>>(
      chapterPath(libraryId, mediaItemId, "/generate", target),
      { method: "POST" },
    ),
  );
}

export async function refreshItemChapters(
  libraryId: string,
  mediaItemId: string,
  target?: ChapterTarget,
): Promise<RefreshedItemChapters> {
  const data = await unwrap(
    request<ApiEnvelope<RefreshedItemChapters | ItemChapter[]>>(
      chapterPath(libraryId, mediaItemId, "/refresh", target),
      { method: "POST" },
    ),
  );
  return Array.isArray(data)
    ? { chapters: data, fingerprint_refresh_queued: false }
    : data;
}

export async function fetchItemProbeStatus(
  libraryId: string,
  mediaItemId: string,
  season: number,
): Promise<ItemProbeStatus> {
  const path = `/libraries/${libraryId}/items/${mediaItemId}/probe-status?${new URLSearchParams({ season: String(season) })}`;
  return unwrap(request<ApiEnvelope<ItemProbeStatus>>(path));
}

export interface RelaxSuggestion {
  field: string;
  label: string;
  detail: string;
  count: number;
}

export async function relaxLibraryFilter(
  libraryId: string,
  filter: LibraryFilter,
): Promise<{ total_without_filter: number; active: boolean; suggestions: RelaxSuggestion[] }> {
  return unwrap(
    request<ApiEnvelope<{ total_without_filter: number; active: boolean; suggestions: RelaxSuggestion[] }>>(
      `/libraries/${libraryId}/items/relax-filter`,
      { method: "POST", body: JSON.stringify(filter) },
    ),
  );
}

export interface SimilarHit {
  id: string | null;
  kind: string;
  title: string;
  year: number | null;
  tmdb_id: string | null;
  poster_url: string | null;
}

/** 相似推荐（GET /libraries/{id}/items/{itemId}/similar，TMDB similar）。 */
export async function fetchItemSimilar(
  libraryId: string,
  mediaItemId: string,
): Promise<SimilarHit[]> {
  return unwrap(
    request<ApiEnvelope<SimilarHit[]>>(`/libraries/${libraryId}/items/${mediaItemId}/similar`),
  );
}

// ---------------------------------------------------------------------------
// 路径核对 + 根目录归并
// ---------------------------------------------------------------------------

export interface PathMissingRow {
  file_id: string;
  media_id: string;
  path: string;
  file_name: string;
  candidates: string[];
}

export async function previewPathReconciliation(libraryId: string): Promise<{ missing: PathMissingRow[] }> {
  return unwrap(request<ApiEnvelope<{ missing: PathMissingRow[] }>>(`/libraries/${libraryId}/path-reconciliation-preview`));
}

export async function applyPathReconciliations(
  libraryId: string,
  reconciliations: { from: string; to: string }[],
): Promise<{ reconciled: number; errors: string[] }> {
  return unwrap(
    request<ApiEnvelope<{ reconciled: number; errors: string[] }>>(`/libraries/${libraryId}/path-reconciliations`, {
      method: "POST",
      body: JSON.stringify({ reconciliations }),
    }),
  );
}

export async function previewRootConsolidation(libraryId: string): Promise<{ duplicates: { keep: string; merge: string }[] }> {
  return unwrap(request<ApiEnvelope<{ duplicates: { keep: string; merge: string }[] }>>(`/libraries/${libraryId}/root-consolidation-preview`));
}

export async function applyRootConsolidations(
  libraryId: string,
  roots: string[],
): Promise<{ consolidated: number; roots: string[] }> {
  return unwrap(
    request<ApiEnvelope<{ consolidated: number; roots: string[] }>>(`/libraries/${libraryId}/root-consolidations`, {
      method: "POST",
      body: JSON.stringify({ roots }),
    }),
  );
}
