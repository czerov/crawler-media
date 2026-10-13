"use client";

import { useEffect, useState } from "react";
import {
  previewMaintenance,
  updateMaintenanceConfig,
  type JobView,
  type MaintenancePreview,
} from "@/lib/api/jobs";

/** 保留天数契约范围（与后端 `MIN_RETENTION_DAYS..=MAX_RETENTION_DAYS` 一致）。 */
const MIN_RETENTION_DAYS = 1;
const MAX_RETENTION_DAYS = 36500;

const RETENTION_KINDS = new Set(["jobs_retention", "playback_history_retention"]);

function readNumber(value: unknown, fallback: number): number {
  return typeof value === "number" && Number.isFinite(value) ? value : fallback;
}

function readBool(value: unknown, fallback: boolean): boolean {
  return typeof value === "boolean" ? value : fallback;
}

/**
 * 维护任务的参数抽屉（DB8 4.3）。
 *
 * - 保留类任务：保留天数、dry_run、只读预览（scanned/candidates/protected/truncated）。
 * - 数据库维护：VACUUM 高级开关 + 风险说明。
 *
 * 保存时用后端返回的 `payload_token`（或当前 payload）作为 CAS 令牌；
 * 409 表示配置已被其他请求修改，需要刷新后再改。
 */
export function MaintenanceJobSettings({
  job,
  intervalSecs,
  onSaved,
  onError,
}: {
  job: JobView;
  intervalSecs?: number;
  onSaved: () => void | Promise<void>;
  onError?: (message: string) => void;
}) {
  const isRetention = RETENTION_KINDS.has(job.kind);
  const isDatabase = job.kind === "database_maintenance";
  const summary = job.maintenance;

  const [retentionDays, setRetentionDays] = useState(
    readNumber(summary?.retention_days, 90),
  );
  const [dryRun, setDryRun] = useState(readBool(summary?.dry_run, false));
  const [vacuumEnabled, setVacuumEnabled] = useState(
    readBool(summary?.vacuum_enabled, false),
  );
  const [saving, setSaving] = useState(false);
  const [previewing, setPreviewing] = useState(false);
  const [preview, setPreview] = useState<MaintenancePreview | null>(null);

  // 服务端刷新后（保存成功 / 定时轮询）同步回真实值。
  useEffect(() => {
    setRetentionDays(readNumber(summary?.retention_days, 90));
    setDryRun(readBool(summary?.dry_run, false));
    setVacuumEnabled(readBool(summary?.vacuum_enabled, false));
  }, [job.id, summary?.retention_days, summary?.dry_run, summary?.vacuum_enabled]);

  const draftPayload = (): Record<string, unknown> => {
    if (isDatabase) {
      return { version: summary?.version ?? 1, vacuum_enabled: vacuumEnabled };
    }
    return { version: summary?.version ?? 1, retention_days: retentionDays, dry_run: dryRun };
  };

  const token = (): Record<string, unknown> => {
    if (job.payload_token) {
      try {
        const parsed = JSON.parse(job.payload_token);
        if (parsed && typeof parsed === "object") return parsed as Record<string, unknown>;
      } catch {
        // 令牌坏掉时退回 DTO 里的 payload，由后端 CAS 判定冲突。
      }
    }
    return job.payload;
  };

  const report = (message: string) => {
    if (onError) onError(message);
    else alert(message);
  };

  const retentionInvalid =
    isRetention && (retentionDays < MIN_RETENTION_DAYS || retentionDays > MAX_RETENTION_DAYS);

  const handleSave = async () => {
    if (retentionInvalid) {
      report(`保留天数必须在 ${MIN_RETENTION_DAYS}..=${MAX_RETENTION_DAYS} 之间（0 不代表永久保留）`);
      return;
    }
    try {
      setSaving(true);
      await updateMaintenanceConfig(job.id, draftPayload(), token(), intervalSecs);
      await onSaved();
    } catch (e) {
      report("保存维护配置失败: " + (e as Error).message);
    } finally {
      setSaving(false);
    }
  };

  const handlePreview = async () => {
    if (retentionInvalid) {
      report(`保留天数必须在 ${MIN_RETENTION_DAYS}..=${MAX_RETENTION_DAYS} 之间`);
      return;
    }
    try {
      setPreviewing(true);
      setPreview(await previewMaintenance(job.id, draftPayload()));
    } catch (e) {
      report("预览失败: " + (e as Error).message);
    } finally {
      setPreviewing(false);
    }
  };

  return (
    <div className="mt-3 rounded-lg border border-white/10 bg-white/[0.05] p-3 text-xs space-y-3">
      {summary?.invalid && (
        <div className="rounded bg-red-500/10 p-2 text-red-300 border border-red-500/20">
          该任务的维护配置无法解析，已按拒绝执行处理（不会退回更激进的删除策略）：
          <span className="break-all"> {summary.error}</span>
        </div>
      )}

      {isRetention && (
        <div className="space-y-2">
          <div className="font-semibold text-white/90">数据保留</div>
          <label className="flex flex-wrap items-center gap-2 text-white/70">
            保留天数:
            <input
              type="number"
              min={MIN_RETENTION_DAYS}
              max={MAX_RETENTION_DAYS}
              value={retentionDays}
              onChange={(e) => setRetentionDays(parseInt(e.target.value, 10) || 0)}
              className="w-24 rounded border border-white/20 bg-black/40 px-2 py-1 text-xs text-white"
            />
            <span className="text-white/40">
              {MIN_RETENTION_DAYS}..{MAX_RETENTION_DAYS}；永久保留请直接关闭该任务
            </span>
          </label>
          <label className="flex items-center gap-2 text-white/70">
            <input
              type="checkbox"
              checked={dryRun}
              onChange={(e) => setDryRun(e.target.checked)}
            />
            试运行（dry_run）：只统计不删除
          </label>
          <div className="flex flex-wrap items-center gap-2">
            <button
              type="button"
              onClick={handlePreview}
              disabled={previewing}
              className="rounded bg-white/10 hover:bg-white/20 px-3 py-1 text-xs text-white transition disabled:opacity-50"
            >
              {previewing ? "预览中..." : "预览候选"}
            </button>
            {preview && (
              <span className="text-white/70">
                扫描 {preview.scanned} · 候选 {preview.candidates} · 保护 {preview.protected} ·{" "}
                {preview.truncated ? "预算不足（计数为下界，非精确总量）" : "完整窗口"}
              </span>
            )}
          </div>
          {preview?.note && <div className="text-white/40">{preview.note}</div>}
        </div>
      )}

      {isDatabase && (
        <div className="space-y-2">
          <div className="font-semibold text-white/90">空间回收（高级）</div>
          <label className="flex items-center gap-2 text-white/70">
            <input
              type="checkbox"
              checked={vacuumEnabled}
              onChange={(e) => setVacuumEnabled(e.target.checked)}
            />
            启用 VACUUM 空间回收
          </label>
          <div className="rounded border border-amber-500/20 bg-amber-500/10 p-2 text-amber-200/90 leading-relaxed">
            风险说明：VACUUM 会重建整个数据库文件，需要独占锁与额外磁盘空间（约等于数据库主体大小），
            在库较大时可能耗时较久，期间相关读写会等待。仅在确实需要向文件系统归还空间时开启；
            该任务本身不做删除，也不会自动删除数据库文件。
          </div>
        </div>
      )}

      {isRetention && (
        <div className="rounded border border-amber-500/20 bg-amber-500/10 p-2 text-amber-200/90 leading-relaxed">
          {job.kind === "playback_history_retention" ? (
            <>
              删除不可撤销，只能从备份恢复；关闭任务只阻止后续批次，不会恢复已删除的数据。
              启用后，历史页与依赖播放历史的统计窗口都只剩保留期内的数据（不是仅隐藏列表）；
              被删除日志对应的全时期统计不会保留。
            </>
          ) : (
            <>
              删除不可撤销，只能从备份恢复；关闭任务只阻止后续批次，不会恢复已删除的数据。
              每个任务定义的最新一条终态记录会被保留，当前排队/运行中的任务不会被删除。
            </>
          )}
        </div>
      )}

      <div className="flex items-center gap-2">
        <button
          type="button"
          onClick={handleSave}
          disabled={saving || retentionInvalid}
          className="rounded bg-sky-500 hover:bg-sky-600 px-3 py-1 text-xs font-semibold text-white transition disabled:opacity-50"
        >
          {saving ? "保存中..." : "保存维护配置"}
        </button>
        <span className="text-white/40">
          配置只影响下次执行；当前运行中的任务继续用它启动时的策略快照。
          维护周期范围 3600..=31536000 秒（在卡片上点周期即可修改）。
        </span>
      </div>
    </div>
  );
}
