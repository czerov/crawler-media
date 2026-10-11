import type { Subscription } from "./api/subscriptions";

/** Keep the server's reason authoritative; tolerate older cached responses. */
export function targetLibraryNotice(
  subscription: Partial<Pick<Subscription, "target_library_status" | "target_library_error">>,
): string | null {
  if (subscription.target_library_status !== "missing" && subscription.target_library_status !== "deleting") {
    return null;
  }
  return subscription.target_library_error || (subscription.target_library_status === "missing"
    ? "目标媒体库已不存在，请重新选择媒体库。"
    : "目标媒体库正在删除或已冻结，请重新选择媒体库。");
}
