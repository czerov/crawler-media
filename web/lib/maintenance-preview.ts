import type { MaintenanceReport } from "@/lib/api/jobs";

/** 只接受和当前草稿完全一致的预览；草稿一变，旧结果立刻作废。 */
export function acceptPreview<T>(currentSignature: string, responseSignature: string, preview: T): T | null {
  return currentSignature === responseSignature ? preview : null;
}

export function maintenanceReportSummary(report: MaintenanceReport | null): string | null {
  if (!report) return null;
  const parts: string[] = [];
  if (typeof report.deleted === "number") parts.push(`删除 ${report.deleted}`);
  if (typeof report.protected === "number") {
    parts.push(
      report.protected_is_lower_bound
        ? `保护 ${report.protected}（下界）`
        : `保护 ${report.protected}`,
    );
  }
  if (report.budget_exhausted) parts.push("预算用尽");
  const skipped = (report.databases ?? [])
    .map((entry) => entry.skip_reason)
    .filter((reason): reason is string => typeof reason === "string" && reason.length > 0);
  if (skipped.length > 0) parts.push(`跳过：${skipped.join("；")}`);
  if (report.note) parts.push(report.note);
  return parts.length > 0 ? parts.join(" · ") : null;
}
