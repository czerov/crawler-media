import assert from "node:assert/strict";
import test from "node:test";
import { readFile } from "node:fs/promises";
import ts from "typescript";
import { waitForLibraryRemoval, removalLabel } from "../lib/library-removal.ts";

const operation = (state) => ({ operation_id: "op", library_id: "lib", state, removed_ledger_count: 0, error: null });

test("accepted removal stays pending through draining and cleaning until success", async () => {
  const states = ["draining", "cleaning", "succeeded"];
  const seen = [];
  const result = await waitForLibraryRemoval(operation("frozen"), async () => operation(states.shift()), {
    intervalMs: 0, onProgress: (value) => seen.push(value.state),
  });
  assert.equal(result.state, "succeeded");
  assert.deepEqual(seen, ["frozen", "draining", "cleaning", "succeeded"]);
});

test("persisted operation resumes and failed operation reports backend error", async () => {
  await assert.rejects(waitForLibraryRemoval(operation("cleaning"), async (id) => {
    assert.equal(id, "op");
    return { ...operation("failed"), error: "cleanup failed" };
  }, { intervalMs: 0 }), /cleanup failed/);
});

test("initial failed operation on retry polls status and can resolve to success", async () => {
  const states = ["draining", "succeeded"];
  const seen = [];
  const result = await waitForLibraryRemoval(
    { ...operation("failed"), error: "old timeout" },
    async () => operation(states.shift()),
    { intervalMs: 0, onProgress: (value) => seen.push(value.state) }
  );
  assert.equal(result.state, "succeeded");
  assert.deepEqual(seen, ["failed", "draining", "succeeded"]);
});

test("abort cancels pending timer without another status request", async () => {
  const controller = new AbortController();
  let requested = false;
  const pending = waitForLibraryRemoval(operation("draining"), async () => {
    requested = true;
    return operation("succeeded");
  }, { signal: controller.signal });
  controller.abort();
  await assert.rejects(pending, { name: "AbortError" });
  assert.equal(requested, false);
});

test("pre-aborted removal does not fetch status or publish progress", async () => {
  const controller = new AbortController();
  controller.abort();
  await assert.rejects(waitForLibraryRemoval(operation("frozen"), async () => {
    assert.fail("must not fetch");
  }, { signal: controller.signal, onProgress: () => assert.fail("must not publish") }), { name: "AbortError" });
});

async function loadApi(request) {
  const source = await readFile(new URL("../lib/api/libraries.ts", import.meta.url), "utf8");
  const js = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText;
  const module = { exports: {} };
  new Function("require", "module", "exports", js)((name) => {
    if (name === "@/lib/http") return { request };
    if (name === "@/lib/library-removal") return { waitForLibraryRemoval };
    if (name === "@/lib/library-filter") return {};
    throw new Error(`Unexpected import ${name}`);
  }, module, module.exports);
  return module.exports;
}

test("list API preserves removal JSON for reload resume", async () => {
  const removal = operation("failed");
  const api = await loadApi(async () => ({ ok: true, data: [{ id: "lib", removal }] }));
  assert.deepEqual((await api.listLibraries())[0].removal, removal);
});

test("list API preserves strm file statistics for UI consumers", async () => {
  const api = await loadApi(async () => ({ ok: true, data: [{ id: "lib", stats: {
    item_count: 2, file_count: 3, strm_file_count: 2, total_size_bytes: 4096,
  } }] }));
  assert.equal((await api.listLibraries())[0].stats.strm_file_count, 2);
});

test("DELETE acceptance waits for terminal GET and forwards cancellation signal", async () => {
  const controller = new AbortController();
  const requests = [];
  const api = await loadApi(async (path, init) => {
    requests.push({ path, init });
    return { ok: true, data: operation(init.method === "DELETE" ? "draining" : "succeeded") };
  });
  const result = await api.removeLibraryAndWait("lib", { signal: controller.signal, intervalMs: 0 });
  assert.equal(result.state, "succeeded");
  assert.deepEqual(requests.map(({ path }) => path), ["/libraries/lib", "/library-removals/op"]);
  assert.ok(requests.every(({ init }) => init.signal === controller.signal));
});

test("both views wire lifecycle status and persistent retry UI", async () => {
  for (const name of ["library-manage-view", "library-detail-view"]) {
    const source = await readFile(new URL(`../components/${name}.tsx`, import.meta.url), "utf8");
    assert.match(source, /useLibraryRemoval\(libraries/);
    assert.match(source, /<LibraryRemovalNotice/);
    assert.doesNotMatch(source, /removeLibraryAndWait/);
  }
  const detail = await readFile(new URL("../components/library-detail-view.tsx", import.meta.url), "utf8");
  assert.match(detail, /canManageLibraries \? \{ manage: true \}/);
  const hook = await readFile(new URL("../lib/use-library-removal.ts", import.meta.url), "utf8");
  assert.match(hook, /controller\.abort\(\)/);
  assert.match(hook, /if \(controller\.signal\.aborted\) return;/);
});

test("removal states have visible deleting and failed copy", () => {
  assert.match(removalLabel(operation("draining")), /正在删除/);
  assert.match(removalLabel({ ...operation("failed"), error: "busy" }), /删除失败.*busy/);
});
