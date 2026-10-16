import type { DownloadTask } from "@/lib/api/downloaders";
import type {
  RuleSetAtom,
  RuleSetSpec,
  Subscription,
  SubscriptionStatus,
  TodaySubscriptionArrival,
} from "@/lib/api/subscriptions";

/**
 * 新契约 RuleSet.atoms → 旧 spec 形状（规则摘要芯片 / 洗版目标标签复用）。
 * 原子只含 resolution/source/free/hr/title_match，旧 spec 的体积、做种数、
 * 洗版目标等维度在转换后自然缺省。
 */
export function ruleSetSpecFromAtoms(atoms: RuleSetAtom[]): RuleSetSpec {
  const spec: RuleSetSpec = {};
  const ordered = [...atoms].sort((a, b) => {
    if (a.kind === "site" && b.kind === "site") return b.priority - a.priority;
    if (a.kind === "site") return 1;
    if (b.kind === "site") return -1;
    return a.priority - b.priority;
  });
  const resolutions: string[] = [];
  const mediaSources: string[] = [];
  const videoCodecs: string[] = [];
  const platformsAllow: string[] = [];
  const platformsBlock: string[] = [];
  const groupsAllow: string[] = [];
  const groupsBlock: string[] = [];
  const hdrLevels: string[] = [];
  const hdrBlock: string[] = [];
  const subLangs: string[] = [];
  const audioLangs: string[] = [];
  const sites: string[] = [];

  for (const atom of ordered) {
    if (atom.kind === "resolution" && atom.value) {
      resolutions.push(atom.value);
    } else if (atom.kind === "source" && atom.value) {
      mediaSources.push(atom.value);
    } else if (atom.kind === "video_codec" && atom.value) {
      videoCodecs.push(atom.value);
    } else if (atom.kind === "free") {
      spec.free_only = true;
    } else if (atom.kind === "hr") {
      spec.exclude_hr = true;
    } else if (atom.kind === "hdr" && atom.value) {
      if (atom.exclude) hdrBlock.push(atom.value);
      else hdrLevels.push(atom.value);
    } else if (atom.kind === "size" && atom.value) {
      const [minStr, maxStr] = atom.value.split("-");
      const min = parseInt(minStr, 10);
      const max = parseInt(maxStr, 10);
      if (!isNaN(min) && min > 0) spec.size_min_mb = min;
      if (!isNaN(max) && max > 0) spec.size_max_mb = max;
    } else if (atom.kind === "min_seeders" && atom.value) {
      const seeders = parseInt(atom.value, 10);
      if (!isNaN(seeders) && seeders > 0) spec.min_seeders = seeders;
    } else if (atom.kind === "subtitle_language" && atom.value) {
      subLangs.push(atom.value);
    } else if (atom.kind === "audio_language" && atom.value) {
      audioLangs.push(atom.value);
    } else if (atom.kind === "site" && atom.value) {
      sites.push(atom.value);
    } else if (atom.kind === "wash_target" && atom.value) {
      const parts = atom.value.split(" ");
      if (parts[0]) spec.cutoff_resolution = parts[0];
      if (parts[1]) spec.upgrade_source = parts[1] as RuleSetSpec["upgrade_source"];
    } else if (atom.kind === "upgrade_ladder" && atom.value) {
      spec.upgrade_ladder = atom.value.split(",").filter(Boolean);
    } else if (atom.kind === "title_match" && atom.value) {
      if (atom.exclude) {
        platformsBlock.push(atom.value);
      } else {
        platformsAllow.push(atom.value);
      }
    }
  }

  if (resolutions.length) spec.resolutions = resolutions;
  if (mediaSources.length) spec.media_sources = mediaSources;
  if (videoCodecs.length) spec.video_codecs = videoCodecs;
  if (platformsAllow.length) spec.platforms = platformsAllow;
  if (platformsBlock.length) spec.platforms_block = platformsBlock;
  if (hdrLevels.length) spec.hdr_levels = hdrLevels;
  if (hdrBlock.length) spec.hdr_block = hdrBlock;
  if (subLangs.length) spec.subtitle_languages_require = subLangs;
  if (audioLangs.length) spec.audio_languages_require = audioLangs;
  if (sites.length) spec.sites = sites;

  return spec;
}

/**
 * 订阅状态的展示元数据与进度文案（订阅页海报墙、详情页操作区共用）。
 * 颜色语义：蓝=追踪中、绿=已收齐、黄=已暂停。
 */
export const subscriptionStatusMeta: Record<
  SubscriptionStatus,
  { label: string; color: string }
> = {
  active: { label: "追踪中", color: "#6aa7ff" },
  completed: { label: "已收齐", color: "#4ade80" },
  paused: { label: "已暂停", color: "#f5c451" },
};

/**
 * 由新契约字段派生订阅状态（后端不再下发 status）。
 * 剧集「收齐」= 缺口为零且期望集合不再生长（不追新季）——与
 * subscriptionFullyCollected 同一判定。
 */
export function subscriptionStatus(
  sub: Pick<Subscription, "follow_future" | "media" | "progress" | "tracking_state">,
): SubscriptionStatus {
  if (sub.tracking_state === "paused") return "paused";
  if (sub.tracking_state === "completed") return "completed";
  if (sub.media.kind === "movie") return sub.progress.imported > 0 ? "completed" : "active";
  // 剧集只有在总集数已知（total > 0）且已全部入库（imported >= total 且 missing === 0）时才能算 completed（已收齐）！
  // 如果 total === 0（未开播或待定档）或 imported === 0（一集没下），绝对不能判定为已收齐！
  if (sub.progress.total > 0 && sub.progress.imported >= sub.progress.total && sub.progress.missing === 0 && !sub.follow_future) {
    return "completed";
  }
  return "active";
}

/** 进度说明：回答「还缺多少 / 入库了多少」——订阅信息里最高频的一眼答案。 */
export function subscriptionProgressNote(sub: Subscription): string {
  const { missing, grabbing, imported, total } = sub.progress;
  const status = subscriptionStatus(sub);
  if (status === "paused") return "暂停追踪";
  const inPipeline = grabbing;
  if (missing === 0) {
    if (sub.media.kind === "movie") {
      return imported > 0 ? "已入库" : inPipeline > 0 ? "下载安排中" : "已收齐";
    }
    if (inPipeline > 0) return `${inPipeline} 集下载中 · 已入库 ${imported}`;
    if (status === "active") return "等待新集播出";
    return imported > 0 ? `全部 ${total} 集已入库` : `全部 ${total} 集已安排`;
  }
  if (sub.media.kind === "movie") return "正在寻找资源";
  const detail = [
    inPipeline > 0 ? `${inPipeline} 集下载中` : null,
    imported > 0 ? `已入库 ${imported}` : null,
  ].filter(Boolean);
  return detail.length > 0 ? `缺 ${missing} 集 · ${detail.join(" · ")}` : `缺 ${missing} 集`;
}

/** 订阅是不是已经全部到手了（issue #221 的核心问题：这部到底入库没有）。
 *
 *  电影是二元的——一部影片只有一个单元，``imported > 0`` 就是"到手了"，
 *  与 ``subscriptionProgressNote`` 判定同源。剧集是集合，"收齐"另有定义：
 *  ``completed`` = 缺口为零且期望集合不再生长（见 SubscriptionStatus），
 *  追新剧因此永远不算收齐——它确实还没完，这正是不能用一个"已入库"标签
 *  笼统盖住两类内容的原因。
 */
export function subscriptionFullyCollected(
  sub: Pick<Subscription, "follow_future" | "media" | "progress">,
): boolean {
  if (sub.media.kind === "movie") return sub.progress.imported > 0;
  // 必须总集数大于 0，且实际入库数达到总集数，才算完全收齐
  return (
    sub.progress.total > 0 &&
    sub.progress.imported >= sub.progress.total &&
    sub.progress.missing === 0 &&
    !sub.follow_future
  );
}

/**
 * 海报角标：**状态优先于能力**。
 *
 * 订阅墙此前只讲能力（「自动续订」），不讲状态——已经入库的电影和还在满世界
 * 找资源的电影长得一模一样，用户扫一遍看不出哪些已经到手（issue #221）。
 * 收齐是扫墙时最先要的那个答案，让它占角标；「自动续订」在一部已经收齐的
 * 完结剧上不再有信息量，让位。
 *
 * 用词沿用项目既有口径，不新造词：电影说「已入库」（与全站 libraryStatus
 * 斜标同词），剧集说「已收齐」（与 subscriptionStatusMeta.completed 同词），
 * 因为这确实是两件事——一部影片是入库，一整季是收齐。
 */
export function subscriptionRibbon(
  sub: Pick<Subscription, "follow_future" | "media" | "progress">,
): { label: string; tone: "owned" | "subscribed" } | undefined {
  if (subscriptionFullyCollected(sub)) {
    return {
      label: sub.media.kind === "movie" ? "已入库" : "已收齐",
      tone: "owned",
    };
  }
  if (sub.media.kind === "tv" && sub.follow_future) {
    return { label: "自动续订", tone: "subscribed" };
  }
  return undefined;
}

export interface SubscriptionCollectionMeta {
  label: string;
  value: string;
  /** 启用中的未完结追更：海报在收录集数前显示静态绿点。 */
  tracking: boolean;
  /** 有单元在洗版：无绿点时以青点呈现（洗版专属色，与详情页同源）。 */
  upgrading?: boolean;
  /** 洗版中的单元数：青点旁的「洗 N」文字——只有一个无图例的点用户
   *  感知不到订阅还在洗版（真实反馈），数量让状态可读且可对账。 */
  upgradingCount?: number;
}

/**
 * 把订阅压成海报内的一行短信息：剧集展示覆盖季 + 收录进度。
 * 新契约不再下发按季库存（season_collection / selected_seasons），
 * 季号取 coverage.season，进度用订阅级 progress 快照。
 */
export function subscriptionCollectionMeta(
  sub: Pick<Subscription, "coverage" | "follow_future" | "media" | "progress" | "tracking_state">,
): SubscriptionCollectionMeta | undefined {
  if (sub.media.kind !== "tv") return undefined;
  const season = sub.coverage.kind === "tv" ? sub.coverage.season : 0;
  const { total, imported, missing } = sub.progress;
  const status = subscriptionStatus(sub);
  // 空窗口（还没播出任何一集）且仍在追：说「待播出」，不说「已收齐」。
  // 待开播/未定档的剧集（总集数为 0 或 1 且首播未定/未开播），不显示「0 / 1」，而是明确显示「待开播」或「待定档」
  const isUnreleased =
    total <= 1 &&
    imported === 0 &&
    missing === 0;

  const value = isUnreleased
    ? "待开播"
    : total > 0
      ? `${imported} / ${total}`
      : missing > 0
        ? `缺 ${missing} 集`
        : imported > 0
          ? `已入库 ${imported} 集`
          : "待开播";
  return {
    label: season === 0 ? "特别篇" : `第 ${season} 季`,
    value,
    tracking: status === "active",
  };
}

export interface TodayArrivalPresentation {
  statusLabel: "预计入库" | "等待资源" | "下载中" | "整理中";
  timeLabel: string;
  estimatedAt: number | null;
}

export interface PresentedTodayArrival {
  arrival: TodaySubscriptionArrival;
  presentation: TodayArrivalPresentation;
}

export interface TodayArrivalGroup {
  subscriptionId: string;
  mediaTitle: string;
  episodeLabel: string;
  episodeCount: number;
  firstWantedId: string;
  /** 距今天几天（0=今天）；后端已把整批候选收敛到同一天。 */
  daysAhead: number;
  presentation: TodayArrivalPresentation;
}

function paddedUnit(value: number): string {
  return String(value).padStart(2, "0");
}

function episodeRanges(episodes: number[]): string {
  const unique = [...new Set(episodes)].toSorted((left, right) => left - right);
  const ranges: Array<{ start: number; end: number }> = [];
  for (const episode of unique) {
    const previous = ranges.at(-1);
    if (previous && episode === previous.end + 1) {
      previous.end = episode;
    } else {
      ranges.push({ start: episode, end: episode });
    }
  }
  return ranges
    .map(({ start, end }) =>
      start === end ? `E${paddedUnit(start)}` : `E${paddedUnit(start)}–E${paddedUnit(end)}`,
    )
    .join("、");
}

/**
 * 同一订阅同日更新的集数合并成一条紧凑的季集范围。
 * 电影的工单是 (0,0) 哨兵，季集号对用户没有意义，直接标“电影”。
 */
function groupedEpisodeLabel(rows: PresentedTodayArrival[]): string {
  if (rows[0].arrival.media_kind === "movie") return "电影";
  const bySeason = new Map<number, number[]>();
  for (const { arrival } of rows) {
    const episodes = bySeason.get(arrival.season_number) ?? [];
    episodes.push(arrival.episode_number);
    bySeason.set(arrival.season_number, episodes);
  }
  return [...bySeason.entries()]
    .toSorted(([left], [right]) => left - right)
    .map(([season, episodes]) => `S${paddedUnit(season)}${episodeRanges(episodes)}`)
    .join(" · ");
}

const arrivalStageOrder: Record<TodayArrivalPresentation["statusLabel"], number> = {
  预计入库: 0,
  等待资源: 0,
  下载中: 1,
  整理中: 2,
};

/**
 * 一部剧的多集以完成最慢的一集为整组状态：未知时间优先于已知时间，
 * 同阶段则取更晚的预计时间，避免部分集已下载时把整组过早显示为整理中。
 */
function blockingPresentation(rows: PresentedTodayArrival[]): TodayArrivalPresentation {
  return rows.toSorted((left, right) => {
    const stage =
      arrivalStageOrder[left.presentation.statusLabel] -
      arrivalStageOrder[right.presentation.statusLabel];
    if (stage !== 0) return stage;
    const leftTime = left.presentation.estimatedAt;
    const rightTime = right.presentation.estimatedAt;
    if (leftTime == null) return rightTime == null ? 0 : -1;
    if (rightTime == null) return 1;
    return rightTime - leftTime;
  })[0].presentation;
}

/** 今日区域按订阅聚合，确保同一部剧无论更新多少集都只占一行。 */
export function groupTodayArrivals(rows: PresentedTodayArrival[]): TodayArrivalGroup[] {
  const grouped = new Map<string, PresentedTodayArrival[]>();
  for (const row of rows) {
    const subscriptionId = String(row.arrival.subscription_id);
    const group = grouped.get(subscriptionId) ?? [];
    group.push(row);
    grouped.set(subscriptionId, group);
  }
  return [...grouped.entries()].map(([subscriptionId, group]) => ({
    subscriptionId,
    mediaTitle: group[0].arrival.media_title,
    episodeLabel: groupedEpisodeLabel(group),
    episodeCount: group.length,
    firstWantedId: String(group[0].arrival.wanted_id),
    daysAhead: group[0].arrival.days_ahead,
    presentation: blockingPresentation(group),
  }));
}

function parsedTime(value: string | null | undefined): number | null {
  if (!value) return null;
  const timestamp = Date.parse(value);
  return Number.isFinite(timestamp) ? timestamp : null;
}

function localDayKey(value: Date): string {
  return `${value.getFullYear()}-${value.getMonth()}-${value.getDate()}`;
}

function formatClock(value: Date): string {
  return new Intl.DateTimeFormat("zh-CN", {
    hour: "2-digit",
    minute: "2-digit",
    hour12: false,
  }).format(value);
}

/**
 * 站点日历日（YYYY-MM-DD）转“M月D日”。按字段切分而不是 Date.parse——
 * 后者把纯日期当 UTC 零点解析，负时区浏览器会整整早显示一天。
 */
function formatCalendarDay(day: string): string | null {
  const [, month, date] = day.split("-").map(Number);
  return Number.isFinite(month) && Number.isFinite(date) ? `${month}月${date}日` : null;
}

/** 首页只展示一个结果时间：同日写时刻，跨到次日时明确标注“明日”。 */
function formatEstimatedArrival(timestamp: number, now: Date): string {
  const value = new Date(timestamp);
  const clock = formatClock(value);
  if (localDayKey(value) === localDayKey(now)) return `约 ${clock}`;
  const tomorrow = new Date(now);
  tomorrow.setDate(tomorrow.getDate() + 1);
  if (localDayKey(value) === localDayKey(tomorrow)) return `明日 ${clock}`;
  return `${value.getMonth() + 1}月${value.getDate()}日 ${clock}`;
}

/**
 * 首次预计时间过期后，以后端调度器给出的下一有效探测点重新计算入库时间；
 * 没有后续探测时不再展示已经失效的旧时间。
 */
function estimatedWantedArrival(
  arrival: TodaySubscriptionArrival,
  predictedAt: number,
  now: Date,
): number | null {
  const importDelay = arrival.estimated_release_to_import_minutes * 60 * 1000;
  const initialEstimate = predictedAt + importDelay;
  if (initialEstimate >= now.getTime()) return initialEstimate;

  const nextProbeAt = parsedTime(arrival.next_probe_at);
  return nextProbeAt == null ? null : Math.max(nextProbeAt, now.getTime()) + importDelay;
}

/**
 * 把后台阶段、出种预测和下载器 ETA 压成首页需要的“状态 + 一个入库时间”。
 * 下载器一旦给出 ETA，就覆盖播出期预测；进度百分比仍留在任务中心。
 */
export function todayArrivalPresentation(
  arrival: TodaySubscriptionArrival,
  task?: DownloadTask,
  now = new Date(),
): TodayArrivalPresentation {
  const taskCompleted = task?.state === "completed";
  if (arrival.status === "downloaded" || taskCompleted) {
    return { statusLabel: "整理中", timeLabel: "即将完成", estimatedAt: now.getTime() };
  }

  if (arrival.status === "grabbed") {
    // 新契约下载任务无 ETA 字段，入库时刻无法推算：保持「下载中」状态，
    // 具体进度回任务中心看
    return {
      statusLabel: "下载中",
      timeLabel: "时间待更新",
      estimatedAt: null,
    };
  }

  const predictedAt = parsedTime(arrival.release_forecast?.predicted_at);
  const forecastUsable = arrival.release_forecast?.confidence !== "volatile";
  const estimatedAt =
    predictedAt == null || !forecastUsable
      ? null
      : estimatedWantedArrival(arrival, predictedAt, now);
  return {
    statusLabel: predictedAt != null && predictedAt <= now.getTime() ? "等待资源" : "预计入库",
    timeLabel:
      estimatedAt == null
        ? pendingTimeLabel(arrival)
        : formatEstimatedArrival(estimatedAt, now),
    estimatedAt,
  };
}

/**
 * 还给不出入库时刻时的兜底文案，尽量比一句“时间待更新”多说一点。
 *
 * 隔天的预告只到“日”这个粒度：探测时刻只有时分，写在几天后的行上会被当成
 * 今天的时刻误读，所以未来行先给日期；今天的行才用探测时刻，让用户看到
 * 系统仍在按计划轮询，而不是停了。
 */
function pendingTimeLabel(arrival: TodaySubscriptionArrival): string {
  if (arrival.days_ahead > 0) {
    const day = formatCalendarDay(arrival.expected_day);
    if (day) return `${day} 播出`;
  }
  const nextProbeAt = parsedTime(arrival.next_probe_at);
  if (nextProbeAt != null) return `${formatClock(new Date(nextProbeAt))} 探测`;
  return "时间待更新";
}
