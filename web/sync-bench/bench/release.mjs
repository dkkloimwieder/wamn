/** Two built shells and two generated bindings against the local WMS host. */
import assert from "node:assert/strict";
import { request } from "node:http";
import { execFileSync } from "node:child_process";
import { mkdir, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright";
import { build, preview } from "vite";
import { outboxDatabase } from "../../runtime/src/sync/names.ts";

const scenario = process.argv[2];
assert.ok(["release", "old-entry"].includes(scenario));
const root = fileURLToPath(new URL("..", import.meta.url));
const scratch = process.env.WAMN_RELEASE_TEST_DIR;
assert.ok(scratch, "the Rust fixture supplies an owned output directory");
const outDir = resolve(scratch, "shell");
const endpoint = new URL(process.env.WAMN_ENDPOINT);
const audience = process.env.WAMN_AUD;
const user = process.env.WAMN_USER_ID;
const locationId = process.env.WAMN_LOCATION_ID;
assert.ok(audience && user && locationId);
const [, , , , application, environment] = audience.split(":");
const database = outboxDatabase(application, environment, user);
let healthy = true;
let holdCreate = false;
let releaseCreate;
let heldCreate;
let server;
let browser;
let result;
const errors = [];

async function buildShell(version) {
  await build({
    root,
    build: { outDir, emptyOutDir: true },
    plugins: [{
      name: "release-test-shell",
      transform(code, id) {
        if (id.endsWith("/src/live.tsx")) {
          return `${code}\ndocument.documentElement.dataset.testShell = ${JSON.stringify(version)};`;
        }
      },
    }],
  });
}

// The proxy handles worker fetches too. It never changes the host's response.
const proxy = {
  name: "release-test-host",
  configurePreviewServer(previewServer) {
    previewServer.middlewares.use(async (incoming, outgoing, next) => {
      const url = new URL(incoming.url, "http://localhost");
      if (url.pathname === "/password/renew") {
        const expires = Math.floor(Date.now() / 1000) + 900;
        outgoing.setHeader("content-type", "application/json");
        outgoing.end(JSON.stringify({ sub: user, expires_at: expires, login_expires_at: expires }));
        return;
      }
      if (!url.pathname.startsWith("/wamn/")) return next();
      if (url.pathname === "/wamn/api/health" && !healthy) {
        outgoing.writeHead(503).end();
        return;
      }
      const chunks = [];
      for await (const chunk of incoming) chunks.push(chunk);
      const body = Buffer.concat(chunks);
      if (holdCreate && url.pathname.endsWith("/packaging/create")) {
        heldCreate = JSON.parse(body)[0];
        await new Promise((resolve) => { releaseCreate = resolve; });
      }
      const headers = { host: process.env.WAMN_ROUTE_HOST, authorization: `Bearer ${process.env.WAMN_BEARER}` };
      for (const name of ["content-type", "if-none-match"]) {
        if (incoming.headers[name]) headers[name] = incoming.headers[name];
      }
      const upstream = request({
        hostname: endpoint.hostname, port: endpoint.port,
        method: incoming.method, path: url.pathname.slice("/wamn".length) + url.search, headers,
      }, (response) => {
        outgoing.writeHead(response.statusCode, response.headers);
        response.pipe(outgoing);
      });
      upstream.on("error", (error) => { errors.push(String(error)); outgoing.writeHead(502).end(); });
      upstream.end(body);
    });
  },
};

try {
  if (scenario === "release") {
    await buildShell("r1");
  } else {
    await mkdir(outDir, { recursive: true });
    const tsconfig = resolve(scratch, "tsconfig.json");
    await writeFile(tsconfig, JSON.stringify({
      extends: resolve(root, "tsconfig.json"),
      compilerOptions: { types: [], paths: {
        "@wamn/web-runtime": [resolve(root, "../runtime/src/index.ts")],
        "@release/r1": [resolve(scratch, "r1.ts")],
        "@release/r2": [resolve(scratch, "r2.ts")],
      } },
      include: [resolve(root, "bench/old-entry.ts")],
    }));
    execFileSync("pnpm", ["exec", "tsc", "--project", tsconfig], { cwd: root, stdio: "inherit" });
    await build({
      root,
      configFile: false,
      resolve: { conditions: ["wfe-source", "browser", "development|production"], alias: [
        { find: "@wamn/web-runtime", replacement: resolve(root, "../runtime/src/index.ts") },
        { find: "@release/r1", replacement: resolve(scratch, "r1.ts") },
        { find: "@release/r2", replacement: resolve(scratch, "r2.ts") },
      ] },
      build: {
        outDir, emptyOutDir: true,
        rollupOptions: {
          input: resolve(root, "bench/old-entry.ts"),
          preserveEntrySignatures: "strict",
          output: { entryFileNames: "old-entry.js" },
        },
      },
    });
    await writeFile(resolve(outDir, "test.html"), "<!doctype html><title>Old entry</title>");
  }
  server = await preview({ root, build: { outDir }, preview: { port: 0, host: "127.0.0.1" }, plugins: [proxy] });
  browser = await chromium.launch({ channel: "chrome", headless: true });
  const page = await browser.newPage();
  page.setDefaultTimeout(60_000);
  page.on("pageerror", (error) => errors.push(String(error)));
  const base = server.resolvedUrls.local[0];
  if (scenario === "old-entry") {
    await page.goto(`${base}test.html`);
    const outcome = await page.evaluate(async (locationId) => (await import("/old-entry.js")).run(locationId), locationId);
    assert.equal(outcome.recorded.packaging_code, 123, "the R1 builder carried its numeric field");
    assert.equal(outcome.entry.request.packagingCode, 123, "R2 kept the saved request");
    assert.equal(outcome.entry.refusal.code, "invalid_input");
    assert.equal(outcome.ordinary.status, "refused");
    assert.equal(outcome.ordinary.code, outcome.entry.refusal.code);
    assert.equal(outcome.accepted.status, "completed");
    assert.deepEqual(errors, []);
    result = { ok: true, scenario, ids: outcome.ids, refusal: outcome.entry.refusal };
  } else {
    await page.goto(`${base}${audience}/live.html?poll=1000`);
    await page.locator(`tr[data-row="${process.env.WAMN_FIRST_ID}"]`).waitFor();
    await page.waitForFunction(() => navigator.serviceWorker.controller !== null);
    const entries = () => page.evaluate(async (name) => new Promise((resolve, reject) => {
      const open = indexedDB.open(name);
      open.onerror = () => reject(open.error);
      open.onsuccess = () => {
        const db = open.result;
        const tx = db.transaction("outbox");
        const read = tx.objectStore("outbox").getAll();
        tx.oncomplete = () => { db.close(); resolve(read.result); };
        tx.onabort = () => { db.close(); reject(tx.error); };
      };
    }), database);
    async function until(test, message) {
      const deadline = Date.now() + 60_000;
      while (!(await test())) {
        if (Date.now() > deadline) throw new Error(message);
        await new Promise((resolve) => setTimeout(resolve, 50));
      }
    }
    async function submit(code) {
      const form = page.getByRole("form", { name: "Create packaging" });
      await form.getByLabel("Packaging code").fill(code);
      await form.getByLabel("Location id").fill(locationId);
      await form.getByRole("button", { name: "Create packaging", exact: true }).click();
      await until(async () => (await form.locator("output").textContent()) === "Recorded", "entry was not recorded");
    }
    await submit("   ");
    await until(async () => (await entries()).some((entry) => entry.state === "refused"), "expected an ordinary refusal");
    healthy = false;
    await submit("RELEASE");
    await until(async () => (await entries()).some((entry) => entry.state === "pending"), "expected a pending entry");
    // Reopen before installing R2 to exercise the saved-outbox load guard.
    await page.reload();
    await page.getByRole("region", { name: "Not uploaded" }).locator("article").waitFor();
    await buildShell("r2");
    await page.evaluate(async () => (await navigator.serviceWorker.getRegistration()).update());
    await page.waitForFunction(() => document.querySelector('[aria-label="Sync status"]')?.textContent.includes("update available, reload"));
    assert.equal(await page.evaluate(() => document.documentElement.dataset.testShell), "r1");
    assert.ok((await entries()).some((entry) => entry.state === "pending"));
    holdCreate = true;
    healthy = true;
    await page.evaluate(() => dispatchEvent(new Event("online")));
    await until(() => releaseCreate !== undefined, "the pending entry did not start sending");
    assert.ok((await entries()).some((entry) => entry.state === "sending"));
    assert.equal(await page.evaluate(() => document.documentElement.dataset.testShell), "r1");
    assert.match(await page.getByLabel("Sync status").textContent(), /update available, reload/);
    holdCreate = false;
    releaseCreate();
    await page.waitForFunction(() => document.documentElement.dataset.testShell === "r2");
    const remaining = await entries();
    assert.equal(remaining.length, 1);
    assert.equal(remaining[0].state, "refused", "a refusal survived and did not block reload");
    assert.deepEqual(errors, []);
    result = { ok: true, scenario, ids: [heldCreate.id], refused: remaining.length };
  }
} catch (error) {
  result = { ok: false, scenario, error: String(error), errors };
} finally {
  releaseCreate?.();
  await browser?.close();
  await server?.close();
}
console.log(JSON.stringify(result));
process.exit(result.ok ? 0 : 1);
