export interface BrowserSettingsPayload {
  cdp: {
    enabled: boolean;
    url: string;
  };
  obscura: {
    install: "download" | "preinstalled";
  };
  user_agent: string;
}

export function formatBrowserPayload(opts: {
  cdpEnabled: boolean;
  cdpUrl: string;
  obscuraInstall: "download" | "preinstalled";
  userAgent: string;
}): BrowserSettingsPayload {
  return {
    cdp: {
      enabled: opts.cdpEnabled,
      url: opts.cdpUrl.trim(),
    },
    obscura: {
      install: opts.obscuraInstall,
    },
    user_agent: opts.userAgent.trim() || "crawler-media/0.1.0",
  };
}

export function resolveBrowserMode(opts: {
  cdpEnabled: boolean;
  cdpUrl?: string | null;
}): "disabled" | "external" | "managed" {
  if (!opts.cdpEnabled) {
    return "disabled";
  }
  if (opts.cdpUrl && opts.cdpUrl.trim().length > 0) {
    return "external";
  }
  return "managed";
}

export function describeBrowserMode(mode: "disabled" | "external" | "managed"): string {
  switch (mode) {
    case "disabled":
      return "已停用（全部请求使用普通 HTTP）";
    case "external":
      return "外部 Chrome CDP（连接用户配置的调试地址）";
    case "managed":
      return "受管 Obscura 浏览器引擎（自动拉起轻量防检测内核）";
  }
}

export function validateSiteProxy(opts: {
  maintenanceVia: "http" | "browser";
  proxy?: string | null;
}): { valid: boolean; error?: string } {
  if (opts.maintenanceVia === "browser" && opts.proxy && opts.proxy.trim().length > 0) {
    return {
      valid: false,
      error: "浏览器维护方式不支持配置代理",
    };
  }
  return { valid: true };
}
