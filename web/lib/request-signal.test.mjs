import test from "node:test";
import assert from "node:assert/strict";
import { getEventListeners } from "node:events";
import { linkRequestSignal } from "./request-signal.mjs";

test("1,000 sequential linkRequestSignal(external) + release() cycles leave 0 listeners", () => {
  const externalController = new AbortController();
  const external = externalController.signal;

  for (let i = 0; i < 1000; i++) {
    const lease = linkRequestSignal(external);
    assert.equal(getEventListeners(external, "abort").length, 1);
    lease.release();
    assert.equal(getEventListeners(external, "abort").length, 0);
  }

  assert.equal(getEventListeners(external, "abort").length, 0);
});

test("an already-aborted external signal yields an aborted lease and adds no listener", () => {
  const externalController = new AbortController();
  const reason = new Error("pre-aborted");
  externalController.abort(reason);
  const external = externalController.signal;

  const lease = linkRequestSignal(external);
  assert.equal(lease.signal.aborted, true);
  assert.equal(lease.signal.reason, reason);
  assert.equal(lease.isLocalTimeout(), false);
  assert.equal(getEventListeners(external, "abort").length, 0);
  lease.release();
  assert.equal(getEventListeners(external, "abort").length, 0);
});

test("a mid-flight external abort aborts the lease and isLocalTimeout() is false", () => {
  const externalController = new AbortController();
  const external = externalController.signal;

  const lease = linkRequestSignal(external, 5000);
  assert.equal(lease.signal.aborted, false);
  assert.equal(getEventListeners(external, "abort").length, 1);

  const customReason = new Error("mid-flight cancellation");
  externalController.abort(customReason);

  assert.equal(lease.signal.aborted, true);
  assert.equal(lease.signal.reason, customReason);
  assert.equal(lease.isLocalTimeout(), false);
  lease.release();
  assert.equal(getEventListeners(external, "abort").length, 0);
});


test("a local timeout (short timeoutMs) aborts the lease and isLocalTimeout() is true", async () => {
  const lease = linkRequestSignal(null, 10);
  assert.equal(lease.signal.aborted, false);

  await new Promise((resolve) => setTimeout(resolve, 30));

  assert.equal(lease.signal.aborted, true);
  assert.equal(lease.isLocalTimeout(), true);
  lease.release();
});
test("release() clears the pending local timer before timeout expires", async () => {
  const lease = linkRequestSignal(null, 10);
  lease.release();

  await new Promise((resolve) => setTimeout(resolve, 30));

  assert.equal(lease.signal.aborted, false);
  assert.equal(lease.isLocalTimeout(), false);
});

test("with external signal, local timeout is not armed even after timeoutMs expires", async () => {
  const externalController = new AbortController();
  const external = externalController.signal;

  const lease = linkRequestSignal(external, 10);
  assert.equal(lease.signal.aborted, false);
  assert.equal(getEventListeners(external, "abort").length, 1);

  await new Promise((resolve) => setTimeout(resolve, 30));

  assert.equal(lease.signal.aborted, false);
  assert.equal(lease.isLocalTimeout(), false);

  lease.release();
  assert.equal(getEventListeners(external, "abort").length, 0);
});

test("release() twice is safe and caller throwing in try/finally leaves 0 listeners", () => {
  const externalController = new AbortController();
  const external = externalController.signal;

  const lease = linkRequestSignal(external);
  assert.equal(getEventListeners(external, "abort").length, 1);

  lease.release();
  assert.equal(getEventListeners(external, "abort").length, 0);
  // Idempotent call
  assert.doesNotThrow(() => lease.release());
  assert.equal(getEventListeners(external, "abort").length, 0);

  // Throwing in try/finally
  assert.throws(() => {
    const errorLease = linkRequestSignal(external);
    try {
      assert.equal(getEventListeners(external, "abort").length, 1);
      throw new Error("caller failure");
    } finally {
      errorLease.release();
    }
  }, /caller failure/);

  assert.equal(getEventListeners(external, "abort").length, 0);
});
