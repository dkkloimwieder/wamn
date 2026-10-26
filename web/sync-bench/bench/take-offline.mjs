/** Online take-offline and counts. The Rust fixture adds 20,000 locations. */
import assert from "node:assert/strict";
import { request } from "node:http";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright";
import { preview } from "vite";
import { rowsDatabase } from "../../runtime/src/sync/names.ts";

const endpoint = new URL(process.env.WAMN_ENDPOINT);
const audience = process.env.WAMN_AUD;
const userId = process.env.WAMN_USER_ID;
const firstId = process.env.WAMN_FIRST_ID;
assert.ok(audience && userId && firstId, "fixture inputs are required");
const [, , , , application, environment] = audience.split(":");
const database = rowsDatabase(application, environment, userId);
const FILL_TIMEOUT_MS = 600_000; // Includes 201 sequential pages on a busy machine.
const reads = { packaging: 0, location: 0, locationChanges: 0 };
const errors = [];
let firstLocationRead;
let result;
const server = await preview({ root: fileURLToPath(new URL("..", import.meta.url)), preview: { port: 0, host: "127.0.0.1" } });
const browser = await chromium.launch({ channel: "chrome", headless: true });
try {
  const context = await browser.newContext();
  const page = await context.newPage();
  page.setDefaultTimeout(60_000);
  page.on("pageerror", (error) => errors.push(String(error)));
  await page.route("**/password/renew", (route) => {
    const expiresAt = Math.floor(Date.now() / 1000) + 15 * 60;
    return route.fulfill({ json: { sub: userId, expires_at: expiresAt, login_expires_at: expiresAt } });
  });
  await page.route("**/wamn/**", async (route) => {
    const incoming = route.request();
    const url = new URL(incoming.url());
    if (url.pathname.endsWith("/packaging/query") && !url.searchParams.has("feed")) reads.packaging++;
    if (url.pathname.endsWith("/location/query")) {
      if (url.searchParams.has("feed")) reads.locationChanges++;
      else { reads.location++; firstLocationRead ??= performance.now(); }
    }
    try {
      const answer = await new Promise((resolve, reject) => {
        const headers = { host: process.env.WAMN_ROUTE_HOST, authorization: `Bearer ${process.env.WAMN_BEARER}` };
        for (const name of ["content-type", "if-none-match"]) {
          if (incoming.headers()[name]) headers[name] = incoming.headers()[name];
        }
        const outgoing = request({ hostname: endpoint.hostname, port: endpoint.port,
          path: `${url.pathname.replace(/^\/wamn/, "")}${url.search}`, method: incoming.method(), headers }, (response) => {
          const chunks = [];
          response.on("data", (chunk) => chunks.push(chunk));
          response.on("end", () => resolve({ status: response.statusCode, headers: response.headers, body: Buffer.concat(chunks) }));
          response.on("error", reject);
        });
        outgoing.on("error", reject);
        const body = incoming.postDataBuffer();
        if (body) outgoing.write(body);
        outgoing.end();
      });
      await route.fulfill({ ...answer, headers: Object.fromEntries(Object.entries(answer.headers)
        .filter(([name]) => !["transfer-encoding", "connection"].includes(name))) });
    } catch (error) {
      errors.push(String(error));
      await route.abort().catch(() => undefined);
    }
  });
  await page.goto(`${server.resolvedUrls.local[0]}${audience}/live.html?poll=1000`);
  await page.locator(`tr[data-row="${firstId}"]`).waitFor();
  await page.getByLabel("Packaging status", { exact: true }).selectOption("available");
  await page.locator(`tr[data-row="${firstId}"]`).waitFor();
  const started = performance.now();
  await page.getByRole("button", { name: "Take offline", exact: true }).click();
  const completion = page.getByRole("status", { name: "Take offline result" });
  await page.waitForFunction(() => document.querySelector('[aria-label="Take offline result"]')?.textContent.includes("Saved."), undefined, { timeout: FILL_TIMEOUT_MS });
  const ended = performance.now();
  const message = await completion.textContent();
  assert.match(message, /Packaging: 1 rows/);
  assert.match(message, /Location: 20002 rows/);
  assert.match(message, /Current as of: \d{4}-/);
  assert.equal(reads.location, 201, "one 100-row page per request, including the two seeded locations");
  const collections = await page.evaluate(async (database) => new Promise((resolve, reject) => {
    const open = indexedDB.open(database);
    open.onerror = () => reject(open.error);
    open.onsuccess = () => {
      const db = open.result;
      const tx = db.transaction("rows");
      const store = tx.objectStore("rows");
      const keys = store.getAllKeys();
      const values = store.getAll();
      tx.oncomplete = () => {
        db.close();
        resolve(keys.result.map((key, i) => ({
          model: JSON.parse(key)[0], scope: JSON.parse(key)[2], count: values.result[i].rows.length,
          bytes: new TextEncoder().encode(JSON.stringify({ key, ...values.result[i] })).byteLength,
          asOf: values.result[i].saved_at,
        })));
      };
      tx.onabort = () => { db.close(); reject(tx.error); };
    };
  }), database);
  const packaging = collections.find((entry) => entry.model === "packaging" && entry.scope === '{"filter":{"status":["available"]}}');
  const location = collections.find((entry) => entry.model === "location");
  assert.equal(packaging.count, 1);
  assert.equal(location.count, 20002);
  assert.deepEqual(errors, []);
  result = { ok: true, packaging, location, reads,
    locationFillMs: ended - firstLocationRead, takeOfflineMs: ended - started,
    measurement: "First location request through durable completion on the screen, including quota and packaging save." };
} catch (error) {
  result = { ok: false, error: String(error), reads, errors };
} finally {
  await browser.close();
  await server.close();
}
console.log(JSON.stringify(result));
process.exit(result.ok ? 0 : 1);
