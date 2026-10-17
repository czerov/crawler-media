import { request } from "@/lib/http";

interface ApiEnvelope<T> {
  ok: boolean;
  data: T;
}

async function unwrap<T>(promise: Promise<ApiEnvelope<T>>): Promise<T> {
  return (await promise).data;
}

/**
 * 任务状态（自有契约 Job.status 枚举的投影；定义见 docs/api-contracts/self.md）。
 * 调度层把「排队/执行中」视为活跃，其余为终态。
 */
export type JobStatus = "queued" | "running" | "succeeded" | "failed" | "cancelled";

/**
 * 任务定义（docs/api-contracts/self.md 的 JobDef）：调度层的持久化定义。
 * 前端任务中心以它为视图模型；运行中/最近一次运行状态见 last_* 字段。
 */
export interface JobDef {
  id: string;
  kind: string;
  name: string;
  friendly_name?: string;
  enabled: boolean;
  schedule: { interval_secs: number } | null;
  payload: Record<string, unknown>;
  concurrency_key: string | null;
  last_status: JobStatus | null;
  last_finished_at: string | number | null;
  last_error?: string | null;
  next_run_after: string | number | null;
  /**
   * DB8 维护任务的可选字段。非维护 def 不返回这些键，因此既有消费方不受影响。
   * 修改令牌 = 保存前读到的 `payload`（不是 payload 内的 schema `version`）。
   */
  maintenance?: MaintenanceSummary;
  payload_token?: string;
  /** 最近一次维护运行报告（`job.progress` 的原始 JSON 字符串，可能为 null）。 */
  last_report?: string | null;
}

/** 维护任务策略摘要（与后端 `MaintenancePolicy` 的 typed 投影一致）。 */
export interface MaintenanceSummary {
  kind: string;
  version?: number;
  /** 保留类任务（任务历史 / 播放历史清理）。 */
  retention_days?: number;
  dry_run?: boolean;
  /** DB-05 分类历史保留：三个独立窗口。 */
  qoe_days?: number;
  transient_cache_days?: number;
  probe_days?: number;
  /** 数据库维护。 */
  vacuum_enabled?: boolean;
  /** payload 损坏时后端显式标记，不假装是默认策略。 */
  invalid?: boolean;
  error?: string;
}

/** 维护任务只读预览结果（有预算；`truncated` 表示计数只是下界）。 */
export interface MaintenancePreview {
  kind: string;
  policy_version?: number;
  retention_days?: number;
  cutoff?: number;
  scanned: number;
  candidates: number;
  protected: number;
  protected_truncated?: boolean;
  truncated: boolean;
  /** DB-05：分类历史保留的窗口检视行数。 */
  examined?: number;
  incomplete?: boolean;
  classes?: Array<{
    class: string;
    cutoff_ms?: number;
    examined?: number;
    candidates?: number;
    protected?: number;
    incomplete?: boolean;
    resume_after?: { at_ms: number; id: string } | null;
  }>;
  note?: string;
}

/** 维护任务最近一次运行报告（`last_report` 解析后的形状，字段均为可选）。 */
export interface MaintenanceReport {
  kind?: string;
  policy_version?: number;
  cutoff?: number | null;
  dry_run?: boolean;
  scanned?: number;
  candidates?: number;
  deleted?: number;
  protected?: number;
  protected_is_lower_bound?: boolean;
  batches?: number;
  budget_exhausted?: boolean;
  /** DB-05：分类历史保留的窗口检视/剩余信号。 */
  examined?: number;
  incomplete?: boolean;
  remaining?: boolean;
  classes?: Array<{
    class: string;
    cutoff_ms?: number;
    examined?: number;
    deleted?: number;
    protected?: number;
    incomplete?: boolean;
    resume_after?: { at_ms: number; id: string } | null;
  }>;
  elapsed_ms?: number;
  databases?: Array<{ skip_reason?: string | null } & Record<string, unknown>>;
  note?: string | null;
}

const MAINTENANCE_KINDS = new Set([
  "database_maintenance",
  "jobs_retention",
  "playback_history_retention",
  "data_retention",
]);

/** 是否是 DB8 维护任务（维护 def 才有参数抽屉与预览）。 */
export function isMaintenanceJob(job: Pick<JobDef, "kind">): boolean {
  return MAINTENANCE_KINDS.has(job.kind);
}

/** 解析 `last_report`；坏 JSON 返回 null，由调用方决定如何提示。 */
export function parseMaintenanceReport(report: string | null | undefined): MaintenanceReport | null {
  if (!report) return null;
  try {
    const parsed = JSON.parse(report);
    return parsed && typeof parsed === "object" ? (parsed as MaintenanceReport) : null;
  } catch {
    return null;
  }
}

/** 任务中心的视图模型 = 任务定义（组件引用名保持旧名 JobView）。 */
export type JobView = JobDef;

/** 活跃（未终结）任务状态集合：用于轮询「还有没有任务在跑」。 */
export const ACTIVE_JOB_STATUSES = new Set<JobStatus>(["queued", "running"]);

/** 是否系统收口的取消：新契约无 cancel_requested_by，仅能按终态判断。 */
export function isSystemCancelled(job: JobView): boolean {
  return job.last_status === "cancelled";
}

/** 列出任务定义（可按 scope=system/all 和 limit 过滤）。 */
export async function listJobs(
  options: {
    scope?: "system" | "all";
    activeOnly?: boolean;
    limit?: number;
  } = {},
): Promise<JobView[]> {
  const query = new URLSearchParams();
  if (options.scope) query.set("scope", options.scope);
  if (options.activeOnly !== undefined) query.set("active_only", String(options.activeOnly));
  if (options.limit !== undefined) query.set("limit", String(options.limit));
  const suffix = query.size > 0 ? `?${query}` : "";
  return unwrap(request<ApiEnvelope<JobDef[]>>(`/jobs${suffix}`));
}

/** 读取单个任务定义。 */
export function getJob(jobId: string): Promise<JobView> {
  return unwrap(request<ApiEnvelope<JobDef>>(`/jobs/${jobId}`));
}

/** 切换任务启用/停用状态。 */
export function toggleJob(jobId: string, enabled: boolean): Promise<{ id: string; enabled: boolean }> {
  return unwrap(
    request<ApiEnvelope<{ id: string; enabled: boolean }>>(`/jobs/${jobId}`, {
      method: "PATCH",
      body: JSON.stringify({ enabled }),
      headers: { "Content-Type": "application/json" },
    }),
  );
}

/** 修改任务执行周期（秒）。 */
export function updateJobSchedule(
  jobId: string,
  intervalSecs: number | null,
): Promise<{ id: string; schedule: { interval_secs: number } | null }> {
  return unwrap(
    request<ApiEnvelope<{ id: string; schedule: { interval_secs: number } | null }>>(
      `/jobs/${jobId}/schedule`,
      {
        method: "PUT",
        body: JSON.stringify({ interval_secs: intervalSecs }),
        headers: { "Content-Type": "application/json" },
      },
    ),
  );
}

/**
 * 保存维护任务配置（DB8）。
 *
 * `expectedPayload` 是并发令牌：保存前从 `GET /jobs` 读到的 `payload`。
 * 与服务端当前值不符时后端返回 409（`job.conflict`），调用方需要刷新后重试。
 * `intervalSecs` 只对维护任务生效，合法范围 3600..=31536000。
 */
export function updateMaintenanceConfig(
  jobId: string,
  payload: Record<string, unknown>,
  expectedPayload: Record<string, unknown>,
  intervalSecs?: number,
): Promise<{
  id: string;
  kind: string;
  payload: Record<string, unknown>;
  payload_token: string;
  schedule: { interval_secs: number } | null;
}> {
  const body: Record<string, unknown> = { payload, expected_payload: expectedPayload };
  if (intervalSecs !== undefined) body.interval_secs = intervalSecs;
  return unwrap(
    request<
      ApiEnvelope<{
        id: string;
        kind: string;
        payload: Record<string, unknown>;
        payload_token: string;
        schedule: { interval_secs: number } | null;
      }>
    >(`/jobs/${jobId}/maintenance-config`, {
      method: "PUT",
      body: JSON.stringify(body),
      headers: { "Content-Type": "application/json" },
    }),
  );
}

/**
 * 维护任务只读预览：只返回有预算的计数，不删除任何数据、不返回播放明细。
 * `truncated=true` 表示固定预算截断了窗口，计数只是下界而非精确总量。
 */
export function previewMaintenance(
  jobId: string,
  payload: Record<string, unknown>,
): Promise<MaintenancePreview> {
  return unwrap(
    request<ApiEnvelope<MaintenancePreview>>(`/jobs/${jobId}/maintenance-preview`, {
      method: "POST",
      body: JSON.stringify({ payload }),
      headers: { "Content-Type": "application/json" },
    }),
  );
}

/** 立即执行一个任务（跳过调度等待）；任务快照由调用方随后刷新。 */
export function runJob(jobId: string): Promise<{ queued: boolean }> {
  return unwrap(
    request<ApiEnvelope<{ queued: boolean }>>(`/jobs/${jobId}/run`, { method: "POST" }),
  );
}

/** 取消排队/运行中的任务；任务快照由调用方随后刷新。 */
export function cancelJob(jobId: string): Promise<{ cancelled: number }> {
  return unwrap(
    request<ApiEnvelope<{ cancelled: number }>>(`/jobs/${jobId}/cancel`, { method: "POST" }),
  );
}

/** 重新执行：新契约无 /retry 端点，等价于立即 run 一次。 */
export function retryJob(jobId: string): Promise<{ queued: boolean }> {
  return runJob(jobId);
}

/**
 * 忽略一条失败任务：新契约无 /dismiss 端点，退化为读取当前定义
 * （组件依赖的返回类型 JobView 不变；静音语义由组件适配阶段另行处理）。
 */
export async function dismissJob(jobId: string, muteSource = false): Promise<JobView> {
  return getJob(jobId);
}

/** 取消「已忽略」：同上，退化为读取当前定义。 */
export async function undismissJob(jobId: string): Promise<JobView> {
  return getJob(jobId);
}

/** 收口失败任务：无批量端点，返回当前处于 failed 的定义列表。 */
export async function dismissAllFailedJobs(): Promise<JobView[]> {
  const jobs = await listJobs();
  return jobs.filter((job) => job.last_status === "failed");
}

/** 手动触发一次调度 tick（开发用）。 */
export function tickJobs(): Promise<Record<string, unknown>> {
  return unwrap(
    request<ApiEnvelope<Record<string, unknown>>>("/jobs/tick", { method: "POST" }),
  );
}
