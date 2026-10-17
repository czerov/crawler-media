import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const readWeb = (relativePath) =>
  readFile(new URL(`../${relativePath}`, import.meta.url), "utf8");

test("jobs API serializes active_only and models run/cancel acknowledgements", async () => {
  const source = await readWeb("lib/api/jobs.ts");

  assert.match(source, /query\.set\("active_only", String\(options\.activeOnly\)\)/);
  assert.match(source, /Promise<\{ queued: boolean \}>/);
  assert.match(source, /Promise<\{ cancelled: number \}>/);
  assert.doesNotMatch(source, /request<ApiEnvelope<JobDef>>\(`\/jobs\/\$\{jobId\}\/(?:run|cancel)`/);
});

test("job action callers refresh snapshots instead of upserting acknowledgements", async () => {
  const [jobCenter, taskCenter] = await Promise.all([
    readWeb("components/job-center.tsx"),
    readWeb("components/task-center-view.tsx"),
  ]);

  assert.doesNotMatch(jobCenter, /upsert\(await cancelJob/);
  assert.match(jobCenter, /await cancelJob\(job\.id\);\s*refresh\(\);/);
  assert.doesNotMatch(taskCenter, /upsert\(await (?:cancelJob|retryJob)/);
  assert.match(taskCenter, /await cancelJob\(job\.id\);\s*refreshJobs\(\);/);
  assert.match(taskCenter, /await retryJob\(job\.id\);\s*refreshJobs\(\);/);
});

test("active job cards never present a previous finish as the active run's end", async () => {
  const source = await readWeb("components/task-center-view.tsx");
  const activeCard = source.match(/function ActiveJobFeedItem\([\s\S]*?\n}\n/);
  assert.ok(activeCard, "active job card exists");
  assert.match(activeCard[0], /job\.last_status === "queued"/);
  assert.match(activeCard[0], /job\.last_status === "running"/);
  assert.doesNotMatch(activeCard[0], /last_finished_at/);
  assert.match(activeCard[0], /即将执行/);
});

test("downloader path maps preserve UI local/remote direction both ways", async () => {
  const [api, component] = await Promise.all([
    readWeb("lib/api/downloaders.ts"),
    readWeb("components/downloader-config-section.tsx"),
  ]);

  assert.match(api, /\{ from: m\.remote, to: m\.local \}/);
  assert.match(component, /\{ local: m\.to, remote: m\.from \}/);
});

test("downloader limits endpoints preserve instance id parameter", async () => {

  const api = await readWeb("lib/api/downloaders.ts");
  assert.match(
    api,
    /export function getDownloaderLimits\(id: string\)[\s\S]*?`\/downloaders\/limits\$\{query\}`/,
    "读取下载器限制必须传递实例 id",
  );
  assert.match(
    api,
    /export function setDownloaderLimits\(\s*id: string[\s\S]*?`\/downloaders\/limits\$\{query\}`/,
    "设置下载器限制必须传递实例 id",
  );
});


test("both manual Torrent delivery paths include the Site torrent id", async () => {
  const [direct, subscribed] = await Promise.all([
    readWeb("lib/api/downloaders.ts"),
    readWeb("lib/api/subscriptions.ts"),
  ]);
  assert.match(direct, /torrent_id: payload\.torrent_id/);
  assert.match(subscribed, /torrent_id: payload\.torrent_id/);
});

test("playback history carries both parts of its composite cursor", async () => {
  const [api, component] = await Promise.all([
    readWeb("lib/api/playback.ts"),
    readWeb("components/playback-stats-section.tsx"),
  ]);

  assert.match(api, /next_cursor_id: string \| null/);
  assert.match(api, /beforeId\?: string \| null/);
  assert.match(api, /params\.set\("before_id", options\.beforeId\)/);
  assert.match(component, /next_cursor_id/);
  assert.match(component, /beforeId/);
});

test("member enabled state comes from the backend and mutations reload it", async () => {
  const [api, component] = await Promise.all([
    readWeb("lib/api/members.ts"),
    readWeb("components/members-section.tsx"),
  ]);

  assert.match(api, /enabled: boolean;/);
  assert.doesNotMatch(api, /TODO: 后端 PATCH \/users\/\{id\}.*enabled/);
  assert.doesNotMatch(component, /enabledById|markEnabled|isEnabled/);
  assert.match(component, /await setMemberStatus\(member\.id, enabling\);\s*await reload\(\);/);
  assert.match(component, /enabled=\{member\.enabled\}/);
});

test("metadata proxy configuration connects API and settings UI", async () => {
  const [api, component] = await Promise.all([
    readWeb("lib/api/metadata.ts"),
    readWeb("components/metadata-settings-section.tsx"),
  ]);

  assert.match(api, /export function getProxySettings/);
  assert.match(api, /export function saveProxySettings/);
  assert.match(api, /export function testProxySettings/);
  assert.match(api, /export function diagnoseProxySettings/);
  assert.match(api, /username\?: string/);
  assert.match(api, /password\?: string/);
  assert.match(component, /saveProxySettings/);
  assert.match(component, /testProxySettings/);
  assert.match(component, /diagnoseProxySettings/);
  assert.match(component, /proxyUsername/);
  assert.match(component, /proxyPassword/);
  assert.match(component, /doubanBypass/);
});

test("settings overview includes metadata network and proxy health panel", async () => {
  const overview = await readWeb("components/settings-overview-section.tsx");
  assert.match(overview, /NetworkProxyHealthPanel/);
  assert.match(overview, /diagnoseProxySettings/);
  assert.match(overview, /元数据网络与代理体检/);
});
