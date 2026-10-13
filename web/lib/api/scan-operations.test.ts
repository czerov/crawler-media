import { describe, it } from "node:test";
import assert from "node:assert/strict";
import type { ScanOperationDetails, ScanOperationResponse } from "@/lib/api/libraries";

describe("scan-operations", () => {
  it("carries 202 accepted fields in ScanOperationResponse", () => {
    const res: ScanOperationResponse = {
      operation_id: "op-123",
      status: "queued",
      queued: true,
      transferred: false,
    };
    assert.equal(res.operation_id, "op-123");
    assert.equal(res.status, "queued");
    assert.equal(res.queued, true);
    assert.equal(res.transferred, false);
  });

  it("carries operation details and report in ScanOperationDetails", () => {
    const details: ScanOperationDetails = {
      operation_id: "op-123",
      status: "completed",
      generation: 1,
      report: { scanned: 10, inserted: 2 },
    };
    assert.equal(details.generation, 1);
    assert.equal(details.status, "completed");
  });
});
