import type { LibraryRemoval } from "./api/libraries";

export interface RemovalWaitOptions {
  signal?: AbortSignal;
  onProgress?: (operation: LibraryRemoval) => void;
  intervalMs?: number;
}

function pause(ms: number, signal?: AbortSignal): Promise<void> {
  return new Promise((resolve, reject) => {
    signal?.throwIfAborted();
    const abort = () => {
      clearTimeout(timer);
      reject(signal?.reason ?? new DOMException("Aborted", "AbortError"));
    };
    const timer = setTimeout(() => {
      signal?.removeEventListener("abort", abort);
      resolve();
    }, ms);
    signal?.addEventListener("abort", abort, { once: true });
  });
}

/** Resume an accepted operation; only terminal success counts as removal. */
export async function waitForLibraryRemoval(
  operation: LibraryRemoval,
  getStatus: (operationId: string, signal?: AbortSignal) => Promise<LibraryRemoval>,
  options: RemovalWaitOptions = {},
): Promise<LibraryRemoval> {
  let isFirst = true;
  for (;;) {
    options.signal?.throwIfAborted();
    options.onProgress?.(operation);
    if (operation.state === "succeeded") return operation;
    if (operation.state === "failed" && !isFirst) {
      throw new Error(operation.error ?? "删除失败，可重新尝试");
    }
    isFirst = false;
    await pause(options.intervalMs ?? 1000, options.signal);
    operation = await getStatus(operation.operation_id, options.signal);
  }
}

export function removalLabel(operation: LibraryRemoval): string {
  if (operation.state === "failed") return `删除失败：${operation.error ?? "请重试"}`;
  if (operation.state === "succeeded") return "已删除媒体库";
  const phase = { frozen: "正在停止相关任务", draining: "正在等待任务结束", cleaning: "正在清除扫描记录与探测缓存" };
  return `正在删除媒体库 · ${phase[operation.state]}`;
}
