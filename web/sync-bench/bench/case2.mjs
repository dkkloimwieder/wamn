/** Case 2: two offline context reopens, three creates, sign-in and a second shell. */
import assert from "node:assert/strict";
import { request } from "node:http";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright";
import { build, preview } from "vite";
import { packagingRead, trackListReads } from "./reads.ts";
import { outboxDatabase, rowsDatabase } from "../../runtime/src/sync/names.ts";

const root = fileURLToPath(new URL("..", import.meta.url));
const scratch = process.env.WAMN_RELEASE_TEST_DIR;
const audience = process.env.WAMN_AUD;
const user = process.env.WAMN_USER_ID;
const locationId = process.env.WAMN_LOCATION_ID;
const firstId = process.env.WAMN_FIRST_ID;
assert.ok(scratch && audience && user && locationId && firstId, "fixture inputs are required");
const endpoint = new URL(process.env.WAMN_ENDPOINT);
const [, , , , application, environment] = audience.split(":");
const outboxName = outboxDatabase(application, environment, user);
const rowsName = rowsDatabase(application, environment, user);
const outDir = resolve(scratch, "case2-shell");
const profile = resolve(scratch, "case2-profile");
const WAIT_MS = 60_000;
const reads = { list: 0, changes: 0, location: 0 };
const session = { renewals: 0, refused: 0, signIns: 0 };
const creates = [];
const echoes = [];
const errors = [];
const steps = [];
const offlineLoads = [];
let ids = [];
let savedEntries;
let offline = false;
let signedIn = true;
let holdReplies = true;
const releaseReplies = [];
let context;
let page;
let server;
let result;

async function buildShell(version) {
  await build({
    root, logLevel: "silent", build: { outDir, emptyOutDir: true },
    plugins: [{
      name: "case2-shell",
      transform(code, id) {
        if (id.endsWith("/src/live.tsx")) {
          return `${code}\ndocument.documentElement.dataset.testShell = ${JSON.stringify(version)};`;
        }
      },
    }],
  });
}

// Use a server proxy so the page and worker reach the same host and session fixture.
const proxy = {
  name: "case2-host",
  configurePreviewServer(previewServer) {
    previewServer.middlewares.use(async (incoming, outgoing, next) => {
      if (offline) { incoming.socket.destroy(); return; }
      const url = new URL(incoming.url, "http://localhost");
      try {
        if (url.pathname === "/password/renew" || url.pathname === "/password/session") {
          for await (const _chunk of incoming) { /* Consume the session request. */ }
          if (url.pathname === "/password/session") { signedIn = true; session.signIns++; }
          else session.renewals++;
          if (!signedIn) { session.refused++; outgoing.writeHead(401).end(); return; }
          const expires = Math.floor(Date.now() / 1000) + 900;
          outgoing.setHeader("content-type", "application/json");
          outgoing.end(JSON.stringify({ sub: user, expires_at: expires, login_expires_at: expires }));
          return;
        }
        if (!url.pathname.startsWith("/wamn/")) return next();
        const read = packagingRead(url);
        if (read !== undefined) reads[read]++;
        if (url.pathname.endsWith("/location/query") && !url.searchParams.has("feed")) reads.location++;
        const chunks = [];
        for await (const chunk of incoming) chunks.push(chunk);
        const body = Buffer.concat(chunks);
        const create = url.pathname.endsWith("/packaging/create");
        if (create) {
          assert.ok(signedIn, "no upload before sign-in");
          creates.push(JSON.parse(body)[0]);
        }
        const answer = await new Promise((resolve, reject) => {
          const headers = { host: process.env.WAMN_ROUTE_HOST, authorization: `Bearer ${process.env.WAMN_BEARER}` };
          for (const name of ["content-type", "if-none-match"]) {
            if (incoming.headers[name]) headers[name] = incoming.headers[name];
          }
          const upstream = request({ hostname: endpoint.hostname, port: endpoint.port,
            method: incoming.method, path: url.pathname.slice("/wamn".length) + url.search, headers }, (response) => {
            const chunks = [];
            response.on("data", (chunk) => chunks.push(chunk));
            response.on("end", () => resolve({ status: response.statusCode, headers: response.headers, body: Buffer.concat(chunks) }));
            response.on("error", reject);
          });
          upstream.on("error", reject);
          upstream.end(body);
        });
        if (read === "changes" && answer.status === 200) {
          const body = JSON.parse(answer.body);
          const value = Array.isArray(body) ? body[0]?.value : body;
          for (const change of value?.rows ?? []) if (!change.deleted && ids.includes(change.id)) echoes.push(change.id);
        }
        if (create) {
          assert.equal(answer.status, 200);
          assert.equal(JSON.parse(answer.body)[0]?.value?.id, JSON.parse(body)[0].id);
          // Keep sending entries until the feed echoes all three accepted rows.
          if (holdReplies) await new Promise((resolve) => releaseReplies.push(resolve));
        }
        outgoing.writeHead(answer.status, answer.headers);
        outgoing.end(answer.body);
      } catch (error) {
        errors.push(String(error));
        outgoing.writeHead(502).end();
      }
    });
  },
};

async function stored(database, store) {
  return page.evaluate(({ database, store }) => new Promise((resolve, reject) => {
    const open = indexedDB.open(database);
    open.onerror = () => reject(open.error);
    open.onsuccess = () => {
      const db = open.result;
      const tx = db.transaction(store);
      const read = tx.objectStore(store).getAll();
      tx.oncomplete = () => { db.close(); resolve(read.result); };
      tx.onabort = () => { db.close(); reject(tx.error); };
    };
  }), { database, store });
}
const entries = () => stored(outboxName, "outbox");
const row = (id) => page.locator(`tr[data-row="${id}"]`);
const status = () => page.getByLabel("Sync status");
async function until(test) {
  const deadline = Date.now() + WAIT_MS;
  while (!(await test())) {
    if (Date.now() >= deadline) throw new Error("condition did not become true within 60 s");
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
}

async function named(name, run) {
  const record = steps.at(-1);
  record.wait = name;
  try {
    const value = await run();
    if (record.ok === false) throw new Error("the step already failed");
    delete record.wait;
    return value;
  } catch (error) {
    throw new Error(`${name}: ${error}`, { cause: error });
  }
}

// Every wait belongs to a named step, including context launches and builds.
async function step(name, run) {
  const startedAt = new Date().toISOString();
  const start = performance.now();
  let timeout;
  const record = { name, startedAt };
  steps.push(record);
  try {
    await Promise.race([(async () => {
      await run();
      if (page && !page.isClosed()) {
        record.counts = {
          outbox: await named("count outbox entries", async () => (await entries()).length),
          packaging: await named("count packaging rows", () => page.locator("tr[data-row]").count()),
          tentative: await named("count tentative rows", () => page.locator("tr[data-row]").filter({ hasText: "tentative" }).count()),
        };
      }
    })(), new Promise((_, reject) => {
      timeout = setTimeout(() => reject(new Error(`${record.wait ?? name}: exceeded 60 s`)), WAIT_MS);
    })]);
    record.ok = true;
  } catch (error) {
    record.ok = false;
    throw new Error(`${name}: ${error}`, { cause: error });
  } finally {
    clearTimeout(timeout);
    record.endedAt = new Date().toISOString();
    record.ms = performance.now() - start;
    record.counts = { ...record.counts, ...reads, sends: creates.length, echoes: echoes.length, ...session };
  }
}

async function openContext(blocked) {
  offline = blocked;
  context = await named("launchPersistentContext", () => chromium.launchPersistentContext(profile,
    { channel: "chrome", headless: true, offline: blocked, timeout: WAIT_MS }));
  context.setDefaultTimeout(WAIT_MS);
  context.setDefaultNavigationTimeout(WAIT_MS);
  await named("install listReads observer", () => context.addInitScript(`(${trackListReads.toString()})(${packagingRead.toString()});`));
  page = context.pages()[0] ?? await named("newPage", () => context.newPage());
  page.on("pageerror", (error) => errors.push(String(error)));
  const assets = [];
  page.on("response", (response) => {
    if (["script", "stylesheet", "font", "image", "manifest"].includes(response.request().resourceType())) {
      assets.push({ url: response.url(), worker: response.fromServiceWorker() });
    }
  });
  const navigation = await named("goto", () => page.goto(`${server.resolvedUrls.local[0]}${audience}/live.html?poll=1000`));
  await named(`first packaging row ${firstId}`, () => row(firstId).waitFor());
  await named("worker controller", () => page.waitForFunction(() => navigator.serviceWorker.controller !== null));
  await named("navigator.onLine assertion", async () => assert.equal(await page.evaluate(() => navigator.onLine), !blocked));
  if (blocked) {
    await named("fonts", () => page.evaluate(() => document.fonts.ready.then(() => undefined)));
    await named("location picker", () => page.getByLabel("Location", { exact: true }).waitFor());
    await named(`location option ${locationId}`, () => page.locator(`select[name="locationId"] option[value="${locationId}"]`).waitFor({ state: "attached" }));
    await named("offline HTML worker assertion", () => assert.ok(navigation.fromServiceWorker(), "offline HTML came from the worker"));
    await named("offline scripts assertion", () => assert.ok(assets.some(({ url }) => url.endsWith(".js")), "offline scripts loaded"));
    await named("offline styles assertion", () => assert.ok(assets.some(({ url }) => url.endsWith(".css")), "offline styles loaded"));
    await named("offline assets worker assertion", () => assert.ok(assets.every(({ worker }) => worker), "all offline assets came from the worker"));
    await named("offline listReads assertion", async () => assert.deepEqual(await page.evaluate(() => window.listReads), { started: 0, resolved: 0 }));
    await named("sign in to upload assertion", async () => assert.match(await status().textContent(), /Sign in to upload/));
    offlineLoads.push({ html: navigation.fromServiceWorker(), assets, listReads: 0 });
  }
}

try {
  await step("build R1 shell", () => buildShell("r1"));
  await step("open R1 online", async () => {
    server = await preview({ root, logLevel: "silent", build: { outDir }, preview: { port: 0, host: "127.0.0.1" }, plugins: [proxy] });
    await openContext(false);
  });
  await step("take packaging and locations offline", async () => {
    await page.getByRole("button", { name: "Take offline", exact: true }).click();
    await page.waitForFunction(() => document.querySelector('[aria-label="Take offline result"]')?.textContent.includes("Saved."));
    const message = await page.getByLabel("Take offline result").textContent();
    assert.match(message, /Packaging: 1 rows/);
    assert.match(message, /Location: 2 rows/);
    const saved = await stored(rowsName, "rows");
    assert.ok(saved.some(({ rows }) => rows.some(({ id }) => id === firstId)));
    assert.ok(saved.some(({ rows }) => rows.some(({ id }) => id === locationId)));
    steps.at(-1).collections = { packaging: 1, location: 2 };
  });
  await step("close online context and reopen offline", async () => {
    await named("close online context", () => context.close());
    signedIn = false;
    await openContext(true);
  });
  await step("record three rows with the offline location picker", async () => {
    const form = page.getByRole("form", { name: "Create packaging" });
    for (let i = 1; i <= 3; i++) {
      await form.getByLabel("Packaging code").fill(`CASE2-${i}`);
      await form.getByLabel("Location", { exact: true }).selectOption(locationId);
      await form.getByRole("button", { name: "Create packaging", exact: true }).click();
      await until(async () => (await entries()).length === i && (await form.locator("output").textContent()) === "Recorded");
    }
    savedEntries = await entries();
    ids = savedEntries.map((entry) => entry.expect.id);
    assert.equal(new Set(ids).size, 3);
    for (const id of ids) assert.match(await row(id).textContent(), /tentative/);
    assert.match(await status().textContent(), /Sign in to upload/);
    assert.equal(creates.length, 0);
    steps.at(-1).outbox = savedEntries.length;
  });
  await step("close and reopen offline with three tentative rows", async () => {
    await named("close offline context", () => context.close());
    await openContext(true);
    // wfe 5a gap wamn-koypi: packagingWrites still owns the local outbox restore.
    for (const id of ids) {
      await row(id).waitFor();
      assert.match(await row(id).textContent(), /tentative/);
    }
    assert.deepEqual(await entries(), savedEntries);
    assert.equal(creates.length, 0);
    steps.at(-1).outbox = 3;
    steps.at(-1).tentative = 3;
  });
  await step("build R2 shell with unchanged request shape", () => buildShell("r2"));
  await step("restore network but refuse renewal", async () => {
    offline = false;
    await context.setOffline(false);
    await until(() => session.refused > 0);
    assert.match(await status().textContent(), /Sign in to upload/);
    assert.deepEqual(await entries(), savedEntries);
    assert.equal(creates.length, 0);
  });
  await step("show R2 update while three entries remain pending", async () => {
    await page.evaluate(async () => (await navigator.serviceWorker.getRegistration()).update());
    await page.waitForFunction(() => document.querySelector('[aria-label="Sync status"]')?.textContent.includes("update available, reload"));
    assert.equal(await page.evaluate(() => document.documentElement.dataset.testShell), "r1");
    assert.deepEqual(await entries(), savedEntries);
  });
  await step("sign in, send three entries and receive three feed echoes", async () => {
    await page.getByLabel("Email", { exact: true }).fill("case2@example.test");
    await page.getByLabel("Password", { exact: true }).fill("case2-test-password");
    await page.getByRole("button", { name: "Sign in", exact: true }).click();
    await until(() => releaseReplies.length === 3 && ids.every((id) => echoes.includes(id)));
    // The feed must reach the local collection before successful replies permit reload.
    await until(async () => (await stored(rowsName, "rows")).some(({ rows }) => ids.every((id) => rows.some((row) => row.id === id))));
    assert.equal(session.signIns, 1);
    assert.equal(creates.length, 3);
    assert.equal(echoes.length, 3);
    for (const entry of savedEntries) {
      const sent = creates.find(({ id }) => id === entry.expect.id);
      assert.equal(sent.packaging_code, entry.request.packagingCode);
      assert.equal(sent.location_id, entry.request.locationId);
      assert.equal(sent.type, entry.request.type);
      assert.equal(sent.status, entry.request.status);
      assert.equal(sent.idempotency_key, entry.request_hash);
      assert.match(await row(entry.expect.id).textContent(), /tentative/);
    }
    assert.ok((await entries()).every((entry) => entry.state === "sending"));
    assert.equal((await entries()).length, 3);
    assert.equal(await page.evaluate(() => document.documentElement.dataset.testShell), "r1");
    assert.match(await status().textContent(), /update available, reload/);
    assert.doesNotMatch(await status().textContent(), /Sign in to upload/);
    steps.at(-1).outbox = 3;
  });
  // wfe gap wamn-c361p: worker activation can stall under load. Keep the 60 s wait.
  await step("R2 reload after the outbox drains (wfe gap wamn-c361p)", async () => {
    holdReplies = false;
    for (const release of releaseReplies.splice(0)) release();
    await page.waitForFunction(() => document.documentElement.dataset.testShell === "r2");
    await until(async () => (await entries()).length === 0);
    for (const id of ids) {
      await row(id).waitFor();
      await until(async () => !(await row(id).textContent()).includes("tentative"));
    }
    assert.equal(creates.length, 3);
    for (const id of ids) assert.equal(echoes.filter((echo) => echo === id).length, 1);
    steps.at(-1).outbox = 0;
    steps.at(-1).tentative = 0;
    assert.deepEqual(errors, []);
  });
  result = { ok: true };
} catch (error) {
  result = { ok: false, error: String(error) };
  const failure = { step: steps.at(-1)?.name, wait: steps.at(-1)?.wait, url: page?.url() ?? null };
  if (page && !page.isClosed()) {
    Object.assign(failure, await page.evaluate(() => {
      const picker = document.querySelector('select[name="locationId"]');
      return {
        url: location.href,
        title: document.title,
        packagingScreen: document.querySelector('[data-shape="a"]') !== null,
        picker: picker !== null,
        optionCount: picker?.options.length ?? 0,
        onLine: navigator.onLine,
        controller: navigator.serviceWorker.controller?.scriptURL ?? null,
        listReads: window.listReads ?? null,
      };
    }).catch((error) => ({ dumpError: String(error) })));
  }
  // Print before the longer result so a truncated result cannot hide the stopped wait.
  console.log(JSON.stringify({ case2Failure: failure }));
  if (page && !page.isClosed()) {
    result.outbox = await entries().catch(String);
    result.worker = await page.evaluate(async () => {
      const registration = await navigator.serviceWorker.getRegistration();
      return { active: registration?.active?.state, waiting: registration?.waiting?.state,
        installing: registration?.installing?.state, shell: document.documentElement.dataset.testShell,
        status: document.querySelector('[aria-label="Sync status"]')?.textContent };
    }).catch(String);
  }
} finally {
  holdReplies = false;
  for (const release of releaseReplies.splice(0)) release();
  await context?.close();
  await server?.close();
}
console.log(JSON.stringify({ ...result, ids, reads, sends: creates.length, echoes, session, steps, offlineLoads, errors }));
process.exit(result.ok ? 0 : 1);
