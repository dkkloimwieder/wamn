/**
 * Live packaging cases: offline reload, corrected refusal, and lost reply.
 * Inputs match follow.mjs. The first argument is offline, refusal or lost-reply.
 * Build sync-bench first. The last stdout line is the JSON result for Rust.
 */
import assert from "node:assert/strict";
import { request } from "node:http";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright";
import { preview } from "vite";
import { packagingRead, trackListReads } from "./reads.ts";
import { outboxDatabase, rowsDatabase } from "../../runtime/src/sync/names.ts";

const scenario = process.argv[2];
assert.ok(["offline", "refusal", "lost-reply"].includes(scenario));
const endpoint = new URL(process.env.WAMN_ENDPOINT);
const audience = process.env.WAMN_AUD;
const userId = process.env.WAMN_USER_ID;
const locationId = process.env.WAMN_LOCATION_ID;
const firstId = process.env.WAMN_FIRST_ID;
assert.ok(audience && userId && locationId && firstId, "fixture inputs are required");
const [, , , , application, environment] = audience.split(":");
const outboxName = outboxDatabase(application, environment, userId);
const rowsName = rowsDatabase(application, environment, userId);
const WAIT_MS = 60_000; // Includes the outbox's 30-second retry interval.

function host(method, path, headers, body) {
  return new Promise((resolve, reject) => {
    const outgoing = request({
      hostname: endpoint.hostname,
      port: endpoint.port,
      method,
      path,
      headers: { ...headers, host: process.env.WAMN_ROUTE_HOST, authorization: `Bearer ${process.env.WAMN_BEARER}` },
    }, (response) => {
      const chunks = [];
      response.on("data", (chunk) => chunks.push(chunk));
      response.on("end", () => resolve({ status: response.statusCode, headers: response.headers, body: Buffer.concat(chunks) }));
      response.on("error", reject);
    });
    outgoing.on("error", reject);
    if (body !== undefined) outgoing.write(body);
    outgoing.end();
  });
}

const reads = { list: 0, changes: 0 };
const creates = [];
const echoes = [];
const errors = [];
const listsPerPage = [];
let offline = false;
let dropped = false;
let result;
const server = await preview({
  root: fileURLToPath(new URL("..", import.meta.url)),
  preview: { port: 0, host: "127.0.0.1" },
});
const browser = await chromium.launch({ channel: "chrome", headless: true });
try {
  const context = await browser.newContext();
  await context.addInitScript(`(${trackListReads.toString()})(${packagingRead.toString()});`);
  const page = await context.newPage();
  page.setDefaultTimeout(WAIT_MS);
  page.on("pageerror", (error) => errors.push(String(error)));
  await page.route("**/password/renew", (route) => {
    if (offline) return route.abort("internetdisconnected");
    const expiresAt = Math.floor(Date.now() / 1000) + 15 * 60;
    return route.fulfill({ json: { sub: userId, expires_at: expiresAt, login_expires_at: expiresAt } });
  });
  await page.route("**/wamn/**", async (route) => {
    if (offline) return route.abort("internetdisconnected");
    const url = new URL(route.request().url());
    const path = `${url.pathname.replace(/^\/wamn/, "")}${url.search}`;
    const read = packagingRead(url);
    const feed = read === "changes";
    if (read !== undefined) reads[read] += 1;
    const create = url.pathname.endsWith("/packaging/create");
    if (create) creates.push(JSON.parse(route.request().postData())[0]);
    const headers = {};
    for (const name of ["content-type", "if-none-match"]) {
      const value = route.request().headers()[name];
      if (value !== undefined) headers[name] = value;
    }
    try {
      const answer = await host(route.request().method(), path, headers, route.request().postDataBuffer() ?? undefined);
      if (feed && answer.status === 200) {
        const body = JSON.parse(answer.body.toString());
        // Feed reads have the generated changes result envelope.
        const value = Array.isArray(body) ? body[0]?.value : body;
        for (const change of value?.rows ?? []) if (!change.deleted) echoes.push(change.id);
      }
      if (create && scenario === "lost-reply" && !dropped) {
        assert.equal(answer.status, 200);
        assert.equal(JSON.parse(answer.body.toString())[0]?.value?.id, creates[0].id);
        dropped = true;
        await route.abort("connectionreset");
        return;
      }
      await route.fulfill({
        status: answer.status,
        headers: Object.fromEntries(Object.entries(answer.headers).filter(([name]) => !["transfer-encoding", "connection"].includes(name))),
        body: answer.body,
      });
    } catch (error) {
      errors.push(String(error));
      await route.abort().catch(() => undefined);
    }
  });

  const row = (id) => page.locator(`[data-shape="a"] tr[data-row="${id}"]`);
  const status = page.getByLabel("Sync status");
  async function stored(database, store) {
    return page.evaluate(async ({ database, store }) => {
      return new Promise((resolve, reject) => {
        const open = indexedDB.open(database);
        open.onerror = () => reject(open.error);
        open.onsuccess = () => {
          const db = open.result;
          const tx = db.transaction(store);
          const read = tx.objectStore(store).getAll();
          tx.oncomplete = () => { db.close(); resolve(read.result); };
          tx.onabort = () => { db.close(); reject(tx.error); };
        };
      });
    }, { database, store });
  }
  async function until(test, message) {
    const end = Date.now() + WAIT_MS;
    while (!(await test())) {
      if (Date.now() >= end) throw new Error(message);
      await new Promise((resolve) => setTimeout(resolve, 50));
    }
  }
  async function submit(code) {
    const form = page.getByRole("form", { name: "Create packaging" });
    await form.getByLabel("Packaging code").fill(code);
    await form.getByLabel("Location id").fill(locationId);
    await form.getByRole("button", { name: "Create packaging", exact: true }).click();
    await until(async () => (await form.locator("output").textContent()) === "Recorded", "create was not recorded");
  }
  async function confirmed(id) {
    await row(id).waitFor();
    await until(async () => !(await row(id).textContent()).includes("tentative"), `row ${id} stayed tentative`);
  }
  async function emptyOutbox() {
    await until(async () => (await stored(outboxName, "outbox")).length === 0, "outbox did not drain");
  }
  async function setOffline(value) {
    offline = value;
    await context.setOffline(value);
    assert.equal(await page.evaluate(() => navigator.onLine), !value);
  }

  await page.goto(`${server.resolvedUrls.local[0]}${audience}/live.html?poll=1000`);
  await row(firstId).waitFor();
  await page.waitForFunction(() => navigator.serviceWorker.controller !== null);
  await until(async () => (await stored(rowsName, "rows"))[0]?.rows.some((row) => row.id === firstId), "initial rows were not persisted");
  assert.equal(reads.list, 1);
  let ids;
  let firstPaint;
  let refusal;

  if (scenario === "offline") {
    await setOffline(true);
    for (let i = 1; i <= 3; i++) await submit(`OFFLINE-${i}`);
    const entries = await stored(outboxName, "outbox");
    assert.equal(entries.length, 3);
    ids = entries.map((entry) => entry.expect.id);
    for (const id of ids) assert.match(await row(id).textContent(), /tentative/);
    assert.equal(creates.length, 0);
    listsPerPage.push(await page.evaluate(() => window.listReads.started));
    await page.evaluate(() => sessionStorage.setItem("hold-list", "yes"));
    const navigation = await page.reload({ waitUntil: "domcontentloaded" });
    assert.ok(navigation.fromServiceWorker(), "offline navigation must come from the worker");
    await row(firstId).waitFor();
    for (const id of ids) {
      await row(id).waitFor();
      assert.match(await row(id).textContent(), /tentative/);
    }
    await page.waitForFunction(() => typeof window.releaseList === "function");
    await page.evaluate(() => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve))));
    firstPaint = await page.evaluate(() => ({ ...window.listReads, offline: !navigator.onLine }));
    assert.deepEqual(firstPaint, { started: 1, resolved: 0, offline: true });
    assert.match(await status.textContent(), /Sign in to upload/);
    assert.equal(creates.length, 0);
    await page.evaluate(() => { sessionStorage.removeItem("hold-list"); window.releaseList(); });
    await page.waitForFunction(() => window.listReads.resolved === 1);
    await setOffline(false);
    await emptyOutbox();
    for (const id of ids) await confirmed(id);
    assert.equal(creates.length, 3);
    assert.deepEqual([...new Set(creates.map((entry) => entry.id))].sort(), [...ids].sort());
    assert.equal(reads.list, 1, "the failed offline list must not cause a list after a write");
  } else if (scenario === "refusal") {
    // WMS trims packaging_code and requires at least one character.
    // Spaces pass the browser's required field and reach host validation.
    await submit("   ");
    const panel = page.getByRole("region", { name: "Not uploaded" });
    const edited = panel.getByLabel("Corrected request");
    await edited.waitFor();
    const entries = await stored(outboxName, "outbox");
    assert.equal(entries.length, 1);
    refusal = entries[0].refusal;
    assert.equal(refusal.code, "invalid_input");
    assert.match(refusal.text, /packaging_code/);
    assert.ok((await panel.textContent()).includes(refusal.text));
    ids = [entries[0].expect.id];
    await edited.fill(JSON.stringify({ ...entries[0].request, packagingCode: "CORRECTED" }));
    await panel.getByRole("button", { name: "Resubmit", exact: true }).click();
    await emptyOutbox();
    await confirmed(ids[0]);
    assert.match(await row(ids[0]).textContent(), /CORRECTED/);
    assert.equal(await panel.locator("article").count(), 0);
    assert.equal(creates.length, 2);
    assert.equal(creates[0].id, creates[1].id);
    assert.notEqual(creates[0].idempotency_key, creates[1].idempotency_key);
  } else {
    await submit("LOST-REPLY");
    await until(() => dropped, "the proxy did not drop a successful create reply");
    ids = [creates[0].id];
    await until(() => echoes.includes(ids[0]), "the feed did not echo the accepted create");
    assert.match(await row(ids[0]).textContent(), /tentative/, "an echo cannot confirm an unsent entry");
    assert.equal((await stored(outboxName, "outbox")).length, 1);
    await emptyOutbox();
    await confirmed(ids[0]);
    assert.equal(creates.length, 2);
    assert.equal(creates[0].idempotency_key, creates[1].idempotency_key);
    assert.notEqual(creates[0].request_id, creates[1].request_id);
    const { request_id: firstRequest, ...first } = creates[0];
    const { request_id: secondRequest, ...second } = creates[1];
    assert.deepEqual(first, second);
  }

  listsPerPage.push(await page.evaluate(() => window.listReads.started));
  assert.deepEqual(listsPerPage, scenario === "offline" ? [1, 1] : [1]);
  assert.equal(reads.list, 1);
  for (const id of ids) assert.equal(echoes.filter((echo) => echo === id).length, 1, `one feed echo for ${id}`);
  const cached = await page.evaluate(async () => {
    const urls = [];
    for (const name of await caches.keys()) {
      for (const request of await (await caches.open(name)).keys()) urls.push(new URL(request.url).pathname);
    }
    return urls;
  });
  assert.ok(cached.includes("/live.html"));
  assert.ok(cached.every((path) => !/^\/(wamn|password)(\/|$)/.test(path)));
  await setOffline(true);
  for (const prefix of ["/wamn", "/password"]) {
    const navigation = await context.newPage();
    await assert.rejects(navigation.goto(`${server.resolvedUrls.local[0]}${prefix.slice(1)}/offline-test`), /net::ERR_/);
    await navigation.close();
  }
  assert.deepEqual(errors, []);
  result = { ok: true, scenario, ids, reads, listsPerPage, firstPaint, refusal, sends: creates.length, echoes, dropped };
} catch (error) {
  result = { ok: false, scenario, error: String(error), reads, listsPerPage, sends: creates.length, echoes, errors };
} finally {
  await browser.close();
  await server.close();
}
console.log(JSON.stringify(result));
process.exit(result.ok ? 0 : 1);
