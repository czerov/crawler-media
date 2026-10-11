import assert from "node:assert/strict";
import test from "node:test";
import { targetLibraryNotice } from "../lib/subscription-target-library.ts";

test("ready or legacy subscriptions have no target warning", () => {
  assert.equal(targetLibraryNotice({ target_library_status: "ready", target_library_error: null }), null);
  assert.equal(targetLibraryNotice({}), null);
});

test("missing target displays server error without clearing the explicit binding", () => {
  const subscription = { library_id: "deleted-id", target_library_status: "missing", target_library_error: "目标媒体库已不存在，请重新选择媒体库。" };
  assert.equal(targetLibraryNotice(subscription), subscription.target_library_error);
  assert.equal(subscription.library_id, "deleted-id");
});

test("frozen targets include a readable reselect instruction even without error text", () => {
  assert.match(targetLibraryNotice({ target_library_status: "deleting", target_library_error: null }), /删除.*重新选择媒体库/);
  assert.match(targetLibraryNotice({ target_library_status: "missing", target_library_error: null }), /不存在.*重新选择媒体库/);
});
