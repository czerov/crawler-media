import type { LibraryRemoval, MediaLibrary } from "@/lib/api/libraries";
import { removalLabel } from "@/lib/library-removal";
import type { RemovalViewState } from "@/lib/use-library-removal";

export function LibraryRemovalNotice({ library, state, onRetry }: {
  library: MediaLibrary;
  state?: RemovalViewState;
  onRetry: (operation?: LibraryRemoval) => void;
}) {
  const operation = state?.operation ?? library.removal;
  if (!state && !operation) return null;
  if (!state?.error && operation?.state === "succeeded") return null;
  const failed = Boolean(state?.error || operation?.state === "failed");
  const pending = state?.pending ?? (operation != null && !failed);
  return (
    <div role={failed ? "alert" : "status"} aria-live="polite" className={`my-2 rounded-xl border px-3 py-2 text-sub ${failed ? "border-red-400/30 bg-red-500/10 text-red-200" : "border-sky-400/25 bg-sky-500/10 text-sky-200"}`}>
      <p className="font-medium">「{library.name}」{state?.error ? `删除未完成：${state.error}` : operation ? removalLabel(operation) : "正在提交删除请求…"}</p>
      <p className="mt-1 text-caption">磁盘文件、NFO 和图片不会删除；观看进度、已看和收藏会保留。</p>
      {!pending && failed && (
        <button type="button" onClick={() => onRetry(operation && operation.state !== "failed" ? operation : undefined)} className="btn-glass mt-2 px-3 py-1 text-caption">
          {operation && operation.state !== "failed" ? "重新读取删除状态" : "重试删除"}
        </button>
      )}
    </div>
  );
}
