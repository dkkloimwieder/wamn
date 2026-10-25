/**
 * The packaging screen against a live host (Epic 2b, docs/plan/wamn-sync.md).
 *
 * The page lists once and then follows the change feed. This driver opens
 * `live.html` in headless Chrome and forwards the page's `/wamn` requests to
 * the host with the caller's token, counting list reads and feed reads. It
 * creates one packaging row through the host's create route, as another
 * client would, and requires the screen to show it with no second list read.
 * It then prints `DELETE <id>` and waits for one line on stdin: the caller
 * deletes that row in the database, and the screen must drop it.
 *
 * Environment: WAMN_ENDPOINT, WAMN_ROUTE_HOST, WAMN_BEARER, WAMN_LOCATION_ID
 * (a location of the scope), WAMN_FIRST_ID (a row the list returns).
 * Run `pnpm run build` first. The last line is the result as JSON.
 */
import { request } from "node:http";
import { createInterface } from "node:readline";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright";
import { preview } from "vite";
import { uuidv7 } from "../../runtime/src/sync/ids.ts";

const endpoint = new URL(process.env.WAMN_ENDPOINT);
const routeHost = process.env.WAMN_ROUTE_HOST;
const bearer = process.env.WAMN_BEARER;
const locationId = process.env.WAMN_LOCATION_ID;
const firstId = process.env.WAMN_FIRST_ID;
/** The follow interval of the page, and how long a change may take: two polls. */
const POLL_MS = 1000;
const WAIT_MS = 2 * POLL_MS + 5000;

/** One request to the host, with the caller's route host and token. */
function host(method, path, headers, body) {
  return new Promise((resolve, reject) => {
    const outgoing = request(
      {
        hostname: endpoint.hostname,
        port: endpoint.port,
        method,
        path,
        headers: { ...headers, host: routeHost, authorization: `Bearer ${bearer}` },
      },
      (response) => {
        const chunks = [];
        response.on("data", (chunk) => chunks.push(chunk));
        response.on("end", () =>
          resolve({ status: response.statusCode, headers: response.headers, body: Buffer.concat(chunks) }),
        );
      },
    );
    outgoing.on("error", reject);
    if (body !== undefined) outgoing.write(body);
    outgoing.end();
  });
}

const reads = { list: 0, changes: 0, notModified: 0 };
const server = await preview({
  root: fileURLToPath(new URL("..", import.meta.url)),
  preview: { port: 0, host: "127.0.0.1" },
});
const browser = await chromium.launch({ channel: "chrome", headless: true });
let result;
try {
  const page = await browser.newPage();
  await page.route("**/wamn/**", async (route) => {
    const url = new URL(route.request().url());
    const path = `${url.pathname.replace(/^\/wamn/, "")}${url.search}`;
    if (url.pathname.endsWith("/packaging/query")) {
      if (decodeURIComponent(url.search).includes("feed")) reads.changes += 1;
      else reads.list += 1;
    }
    const headers = {};
    const condition = route.request().headers()["if-none-match"];
    if (condition !== undefined) headers["if-none-match"] = condition;
    const answer = await host(route.request().method(), path, headers, route.request().postDataBuffer() ?? undefined);
    if (answer.status === 304) reads.notModified += 1;
    await route.fulfill({
      status: answer.status,
      headers: Object.fromEntries(
        Object.entries(answer.headers).filter(([name]) => !["transfer-encoding", "connection"].includes(name)),
      ),
      body: answer.body,
    });
  });
  const row = (id) => page.locator(`[data-shape="a"] tr[data-row="${id}"]`);
  await page.goto(`${server.resolvedUrls.local[0]}live.html?poll=${POLL_MS}`);
  await row(firstId).waitFor({ timeout: 60_000 });
  if (reads.list !== 1) throw new Error(`the screen listed ${reads.list} times before the write`);

  const created = await host(
    "POST",
    "/packaging/create",
    { "content-type": "application/json" },
    JSON.stringify([
      {
        request_id: "follow-create",
        idempotency_key: "follow-create",
        id: uuidv7(),
        packaging_code: "FOLLOW-1",
        type: "pallet",
        location_id: locationId,
        status: "available",
      },
    ]),
  );
  const createdId = JSON.parse(created.body.toString())[0]?.value?.id;
  if (created.status !== 200 || typeof createdId !== "string") {
    throw new Error(`the create answered ${created.status}: ${created.body}`);
  }
  await row(createdId).waitFor({ timeout: WAIT_MS });
  const afterWrite = { ...reads };

  console.log(`DELETE ${createdId}`);
  const input = createInterface({ input: process.stdin });
  await new Promise((resolve) => input.once("line", resolve));
  input.close();
  await row(createdId).waitFor({ state: "detached", timeout: WAIT_MS });
  if (reads.list !== 1) throw new Error(`the screen listed ${reads.list} times`);
  if (afterWrite.changes < 1) throw new Error("the screen showed the write with no feed read");
  result = { ok: true, created: createdId, afterWrite, reads };
} catch (error) {
  result = { ok: false, error: String(error), reads };
} finally {
  await browser.close();
  await server.close();
}
console.log(JSON.stringify(result));
process.exit(result.ok ? 0 : 1);
