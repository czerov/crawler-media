import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { acceptPreview, maintenanceReportSummary } from "./maintenance-preview.ts";

describe("maintenance preview binding", () => {
  it("drops a late response after the draft changes", () => {
    const late = acceptPreview("days:30", "days:90", { candidates: 12 });
    assert.equal(late, null);
  });

  it("keeps a response that still matches the current draft", () => {
    const current = acceptPreview("days:30", "days:30", { candidates: 4 });
    assert.deepEqual(current, { candidates: 4 });
  });
});

describe("maintenance report summary", () => {
  it("shows a protected lower bound and the skip reason", () => {
    const text = maintenanceReportSummary({
      deleted: 500,
      protected: 501,
      protected_is_lower_bound: true,
      databases: [{ skip_reason: "冷却期未到" }],
      note: "任务定义已禁用，未启动下一批",
    });
    assert.equal(
      text,
      "删除 500 · 保护 501（下界） · 跳过：冷却期未到 · 任务定义已禁用，未启动下一批",
    );
  });
});
