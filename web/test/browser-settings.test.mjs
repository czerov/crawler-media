import assert from "node:assert/strict";
import test from "node:test";

import {
  describeBrowserMode,
  formatBrowserPayload,
  resolveBrowserMode,
  validateSiteProxy,
} from "../lib/browser-settings.ts";

test("resolveBrowserMode: 单开关与外部端点解析", () => {
  assert.equal(
    resolveBrowserMode({ cdpEnabled: false, cdpUrl: "http://127.0.0.1:9222" }),
    "disabled"
  );
  assert.equal(
    resolveBrowserMode({ cdpEnabled: true, cdpUrl: "" }),
    "managed"
  );
  assert.equal(
    resolveBrowserMode({ cdpEnabled: true, cdpUrl: "   " }),
    "managed"
  );
  assert.equal(
    resolveBrowserMode({ cdpEnabled: true, cdpUrl: null }),
    "managed"
  );
  assert.equal(
    resolveBrowserMode({ cdpEnabled: true, cdpUrl: "http://127.0.0.1:9222" }),
    "external"
  );
});

test("describeBrowserMode: 模式中文文案明确", () => {
  assert.match(describeBrowserMode("disabled"), /已停用/);
  assert.match(describeBrowserMode("external"), /外部 Chrome CDP/);
  assert.match(describeBrowserMode("managed"), /受管 Obscura/);
});

test("formatBrowserPayload: 构造后端契约结构", () => {
  const payload = formatBrowserPayload({
    cdpEnabled: true,
    cdpUrl: "  http://127.0.0.1:9222  ",
    obscuraInstall: "download",
    userAgent: "CustomUA/1.0",
  });
  assert.deepEqual(payload, {
    cdp: {
      enabled: true,
      url: "http://127.0.0.1:9222",
    },
    obscura: {
      install: "download",
    },
    user_agent: "CustomUA/1.0",
  });
});

test("validateSiteProxy: 浏览器维护互斥代理", () => {
  assert.deepEqual(
    validateSiteProxy({ maintenanceVia: "browser", proxy: "http://proxy:8080" }),
    { valid: false, error: "浏览器维护方式不支持配置代理" }
  );
  assert.deepEqual(
    validateSiteProxy({ maintenanceVia: "http", proxy: "http://proxy:8080" }),
    { valid: true }
  );
  assert.deepEqual(
    validateSiteProxy({ maintenanceVia: "browser", proxy: "" }),
    { valid: true }
  );
  assert.deepEqual(
    validateSiteProxy({ maintenanceVia: "browser", proxy: null }),
    { valid: true }
  );
});
