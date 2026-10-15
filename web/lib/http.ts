import { publicEnv } from "@/lib/env";
import { clearBackdropCache } from "@/lib/backdrop-cache";
import { clearUiPrefsCache } from "@/lib/ui-prefs-cache";
import { linkRequestSignal } from "@/lib/request-signal.mjs";

export class HttpError extends Error {
  status: number;
  details: unknown;

  constructor(message: string, status: number, details: unknown) {
    super(message);
    this.name = "HttpError";
    this.status = status;
    this.details = details;
  }
}

function trimTrailingSlash(value: string): string {
  return value !== "/" && value.endsWith("/") ? value.slice(0, -1) : value;
}

function trimLeadingSlash(value: string): string {
  return value.startsWith("/") ? value.slice(1) : value;
}

/**
 * 把 API 相对路径解析成完整请求地址。
 * `VITE_API_BASE_URL` 默认 `/api/v1`，因此 `request("/libraries")` 打的是
 * `/api/v1/libraries`。后端历史遗留根路径已全部移除，自有接口统一前缀 `/api/v1`。
 */
export function resolveRequestUrl(path: string): string {
  if (/^https?:\/\//.test(path)) {
    return path;
  }

  const baseUrl = trimTrailingSlash(publicEnv.apiBaseUrl);
  const requestPath = trimLeadingSlash(path);

  if (baseUrl === "/") {
    return `/${requestPath}`;
  }

  return `${baseUrl}/${requestPath}`;
}

/** 当前登录 token（localStorage 持久化，login() 写入 / logout() 清除）。 */
export function getAuthToken(): string | null {
  if (typeof window === "undefined") return null;
  return window.localStorage.getItem("mc_token");
}

function buildHeaders(initHeaders?: HeadersInit, body?: BodyInit | null): HeadersInit {
  const headers = new Headers(initHeaders);

  // 自动附加 Bearer token（显式传入的 authorization 优先）。
  const token = getAuthToken();
  if (token && !headers.has("Authorization")) {
    headers.set("Authorization", `Bearer ${token}`);
  }

  headers.set("Accept", "application/json");

  // FormData（文件上传）必须由浏览器自动带上含 boundary 的 multipart Content-Type，
  // 这里绝不能手动设 application/json，否则后端无法解析上传体。
  if (body && !(body instanceof FormData) && !headers.has("Content-Type")) {
    headers.set("Content-Type", "application/json");
  }

  return headers;
}

/**
 * 全站统一的未登录兜底：任何接口返回 401 就跳登录页。
 * /login、/setup 自身除外——登录失败（密码错误也是 401）要留在原页面展示错误。
 * 跳转时把当前地址（路径 + 查询串）编码进 ?next=，登录成功后原样回到用户离开的页面，
 * 避免会话过期后一律被打回首页。注意这只是体验优化，真正的安全边界在后端。
 */
export function redirectToLoginOn401(status: number): void {
  if (status === 401 && typeof window !== "undefined") {
    const path = window.location.pathname;
    // 影片分享页（/s/…）的访客没有账号：401 是「要密码」，由分享页自己接住
    if (path !== "/login" && path !== "/setup" && !path.startsWith("/s/")) {
      const next = encodeURIComponent(path + window.location.search);
      window.localStorage.removeItem("mc_token");
      clearBackdropCache();
      clearUiPrefsCache();
      window.location.href = `/login?next=${next}`;
    }
  }
}

function classifyRequestAbort(lease: { isLocalTimeout: () => boolean }, error: unknown): never {
  if (lease.isLocalTimeout()) {
    throw new HttpError("请求超时，请检查服务状态后重试", 0, null);
  }
  throw error;
}

export async function request<T>(path: string, init: RequestInit = {}): Promise<T> {
  // 兜底超时：后端异常/负载过高时不让页面无限「正在连接服务…」。
  // 调用方传了自己的 signal（AbortController）时不覆盖，由调用方决定。
  // The lease deliberately covers response-body consumption so external cancellation
  // works while the body streams. This also means fallback timeout covers the body read,
  // unlike the previous headers-only boundary.
  const lease = linkRequestSignal(init.signal, 15_000);

  try {
    let response: Response;
    try {
      response = await fetch(resolveRequestUrl(path), {
        ...init,
        signal: lease.signal,
        headers: buildHeaders(init.headers, init.body),
      });
    } catch (error) {
      classifyRequestAbort(lease, error);
    }

    if (response.status === 204) {
      return undefined as T;
    }

    if (!response.ok) {
      redirectToLoginOn401(response.status);
    }

    const contentType = response.headers.get("content-type") || "";
    const isJson = contentType.includes("application/json");
    let payload: unknown;
    try {
      payload = isJson ? await response.json() : await response.text();
    } catch (error) {
      classifyRequestAbort(lease, error);
    }

    if (!response.ok) {
      // Self API error envelope: { ok: false, error: { code, message } }.
      const message =
        isJson && payload && typeof payload === "object"
          ? (("error" in payload && payload.error && typeof payload.error === "object"
              ? String((payload.error as { message?: unknown }).message ?? "")
              : "") ||
            ("message" in payload ? String((payload as { message: unknown }).message) : ""))
          : `Request failed with status ${response.status}`;

      throw new HttpError(message || `Request failed with status ${response.status}`, response.status, payload);
    }

    // Success: return the full envelope; callers unwrap `.data` themselves
    // (both the legacy `{success, code, message, data}` and the self API
    // `{ok, data}` shapes carry `data`, so existing unwrap() calls keep working).
    return payload as T;
  } finally {
    lease.release();
  }
}
