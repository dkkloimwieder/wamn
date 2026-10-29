// The five sync checks of the open receipt lines screen. The Rust fixture starts the real local services.
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { join } from "node:path";
import { createInterface } from "node:readline";

const require = createRequire(new URL("../../../../web/shell/package.json", import.meta.url));
const { chromium } = require("playwright");
const required = (name) => {
  assert.ok(process.env[name], `set ${name}`);
  return process.env[name];
};
const input = createInterface({ input: process.stdin });
const fixture = JSON.parse((await input[Symbol.asyncIterator]().next()).value);
input.close();
assert.ok(fixture.reportDirectory, "the Rust fixture supplies the screenshot directory");
console.error(`Receiving sync screenshots: ${fixture.reportDirectory}`);

const OPEN = "Open receipt lines";
const LOCATION_ID = "00000000-0000-0000-0000-000000000201";
// The collection polls every 10 s. One poll and the time the write takes to land.
const ONE_POLL = 13_000;
const browser = await chromium.launch({ channel: "chrome", headless: true });
const errors = [];
let page;
let step = "sign-in";

const table = (on) => on.getByRole("table", { name: OPEN, exact: true });
const statusLine = (on) => on.getByLabel("Sync status", { exact: true });
const row = (on, order) => table(on).locator("tbody > tr[data-row]").filter({ hasText: order });
const tentative = async (on, order) => (await row(on, order).locator('[data-tentative="true"]').count()) > 0;
const remaining = (on, order) =>
  table(on).evaluate((node, order) => {
    const heads = [...node.querySelectorAll("thead tr:last-child > *")];
    const at = heads.findIndex((cell) => /Remaining/.test(cell.textContent ?? ""));
    const line = [...node.querySelectorAll("tbody > tr[data-row]")].find((r) => r.textContent?.includes(order));
    return Number.parseFloat(line?.children[at]?.textContent ?? "NaN");
  }, order);
const until = async (what, test, timeout) => {
  const deadline = Date.now() + timeout;
  for (;;) {
    if (await test()) return;
    assert.ok(Date.now() < deadline, `${what} within ${timeout} ms`);
    await new Promise((resolve) => setTimeout(resolve, 200));
  }
};
const capture = (on, name) => on.screenshot({ path: join(fixture.reportDirectory, `${name}.png`), fullPage: false });
const watch = (on) => on.on("pageerror", (error) => errors.push(`${step}: ${error.message}`));

/** Every read of the screen's route that is not a feed call is a page read. */
const pageReads = [];
const feedCalls = [];
const count = (context) =>
  context.on("request", (request) => {
    const url = new URL(request.url());
    if (!url.pathname.endsWith("/receiving/load_open_receipt_lines")) return;
    (url.search.includes("feed") ? feedCalls : pageReads).push(url.pathname + url.search);
  });

async function signIn(on) {
  await on.goto(required("WAMN_BROWSER_URL"));
  await on.getByLabel("Email", { exact: true }).fill(required("WAMN_BROWSER_EMAIL"));
  await on.getByLabel("Password", { exact: true }).fill(required("WAMN_BROWSER_PASSWORD"));
  await on.getByRole("button", { name: "Sign in", exact: true }).click();
  await on
    .getByRole("table", { name: "Audiences", exact: true })
    .getByRole("link", { name: "dev", exact: true })
    .click();
  await on
    .getByRole("navigation", { name: "Primary navigation" })
    .getByRole("link", { name: OPEN, exact: true })
    .click();
  await table(on).locator("tbody > tr[data-row]").first().waitFor();
}

/** Record one receipt from the row action. The panel closes once the outbox holds the entry. */
async function record(on, order, reference, quantity) {
  await row(on, order).getByRole("button", { name: "Record a receipt", exact: true }).click();
  const panel = on.getByRole("form", { name: "Record a receipt", exact: true });
  await panel.getByLabel("Receipt reference", { exact: true }).fill(reference);
  await panel.getByLabel("Quantity", { exact: true }).fill(quantity);
  const choice = panel.getByRole("combobox", { name: "Location", exact: true });
  if ((await choice.count()) > 0) await choice.selectOption({ label: "DOCK-BROWSER" });
  else await panel.getByLabel("Location id", { exact: true }).fill(LOCATION_ID);
  await panel.getByRole("button", { name: "Record a receipt", exact: true }).click();
  await panel.waitFor({ state: "detached" });
}

const pending = async (on) => (await statusLine(on).textContent()) ?? "";
const settled = async (on) => /Pending: 0\. Refused: 0\./.test(await pending(on));

try {
  const first = await browser.newContext({ viewport: { width: 1400, height: 900 } });
  count(first);
  page = await first.newPage();
  watch(page);
  await signIn(page);
  const openLines = page.url();
  // The worker controls the page after its first load.
  await page.evaluate(() => navigator.serviceWorker.ready);

  step = "check-1";
  let navigations = 0;
  page.on("framenavigated", (frame) => {
    if (frame === page.mainFrame()) navigations += 1;
  });
  const readsBefore = pageReads.length;
  assert.equal(await remaining(page, "PO-BROWSER-01"), 5);
  await first.setOffline(true);
  const dropped = Date.now();
  await record(page, "PO-BROWSER-01", "SYNC-DROP", "1");
  assert.ok(await tentative(page, "PO-BROWSER-01"), "the receipt recorded during the drop shows tentative");
  assert.equal(await remaining(page, "PO-BROWSER-01"), 4);
  await capture(page, "check-1-drop");
  while (Date.now() - dropped < 30_000) {
    assert.equal(await table(page).locator("tbody > tr[data-row]").count(), 25, "rows keep rendering");
    await new Promise((resolve) => setTimeout(resolve, 2_000));
  }
  await first.setOffline(false);
  await until(
    "the dropped receipt lands",
    async () => !(await tentative(page, "PO-BROWSER-01")) && (await settled(page)),
    30_000,
  );
  assert.equal(await remaining(page, "PO-BROWSER-01"), 4);
  assert.equal(navigations, 0, "the screen never navigates");
  assert.deepEqual(pageReads.slice(readsBefore), [], "the screen never reads its page");
  await capture(page, "check-1-landed");
  console.log(
    `CHECK 1 PASS: 30 s drop, rows kept, receipt tentative then landed, ${feedCalls.length} feed calls, 0 page reads`,
  );

  step = "check-2";
  const second = await browser.newContext({ viewport: { width: 1400, height: 900 } });
  const other = await second.newPage();
  watch(other);
  await signIn(other);
  await record(other, "PO-BROWSER-02", "SYNC-OTHER", "2");
  await until(
    "the other browser's receipt lands",
    async () => !(await tentative(other, "PO-BROWSER-02")) && (await settled(other)),
    30_000,
  );
  const landed = Date.now();
  await until(
    "the first browser shows the other receipt",
    async () => (await remaining(page, "PO-BROWSER-02")) === 3,
    ONE_POLL,
  );
  console.log(`CHECK 2 PASS: another browser's receipt shown after ${Date.now() - landed} ms, remaining 5 -> 3`);
  await capture(page, "check-2-other");
  await second.close();

  step = "check-3";
  await first.setOffline(true);
  await record(page, "PO-BROWSER-03", "SYNC-REOPEN", "1");
  assert.ok(await tentative(page, "PO-BROWSER-03"));
  await page.close();
  page = await first.newPage();
  watch(page);
  await page.goto(openLines);
  await row(page, "PO-BROWSER-03").waitFor();
  assert.equal(await table(page).locator("tbody > tr[data-row]").count(), 25, "saved rows paint");
  await until("the pending receipt shows", () => tentative(page, "PO-BROWSER-03"), 10_000);
  assert.match(await pending(page), /Offline, sign in to upload\./);
  assert.match(await pending(page), /Pending: 1\./);
  await capture(page, "check-3-reopened");
  await first.setOffline(false);
  await until(
    "replay runs on return",
    async () => !(await tentative(page, "PO-BROWSER-03")) && (await settled(page)),
    30_000,
  );
  assert.equal(await remaining(page, "PO-BROWSER-03"), 4);
  assert.doesNotMatch(await pending(page), /Offline/);
  console.log("CHECK 3 PASS: reopened offline, saved rows and pending receipt shown, replayed on return");

  step = "check-4";
  await first.setOffline(true);
  await record(page, "PO-BROWSER-04", "SYNC-FLAP-1", "1");
  await record(page, "PO-BROWSER-05", "SYNC-FLAP-2", "1");
  await record(page, "PO-BROWSER-04", "SYNC-FLAP-3", "1");
  assert.match(await pending(page), /Pending: 3\./);
  for (let flap = 0; flap < 8; flap += 1) {
    await first.setOffline(false);
    await new Promise((resolve) => setTimeout(resolve, 150 + 100 * (flap % 3)));
    await first.setOffline(true);
    await new Promise((resolve) => setTimeout(resolve, 300));
  }
  await first.setOffline(false);
  await until("the outbox empties", () => settled(page), 60_000);
  // A receipt that landed twice would move remaining twice, or be refused.
  await until(
    "each receipt lands once",
    async () => (await remaining(page, "PO-BROWSER-04")) === 3 && (await remaining(page, "PO-BROWSER-05")) === 4,
    ONE_POLL,
  );
  console.log("CHECK 4 PASS: 8 flaps during replay, 3 receipts landed once each, outbox empty");

  step = "check-5";
  await record(page, "PO-BROWSER-01", "SYNC-REFUSED", "99");
  const refused = page.getByRole("region", { name: "Not uploaded", exact: true }).locator("article");
  await refused.first().waitFor({ timeout: 30_000 });
  assert.equal(await refused.count(), 1);
  assert.match(await pending(page), /Refused: 1\./);
  assert.equal(await remaining(page, "PO-BROWSER-01"), 4, "the refused receipt moves nothing");
  await capture(page, "check-5-refused");
  const corrected = refused.getByLabel("Corrected request", { exact: false });
  const request = JSON.parse(await corrected.inputValue());
  const lower = (value) => {
    if (value === null || typeof value !== "object") return;
    for (const [key, inner] of Object.entries(value)) {
      if (key === "quantity") value[key] = "1";
      else lower(inner);
    }
  };
  lower(request);
  await corrected.fill(JSON.stringify(request, null, 2));
  await refused.getByRole("button", { name: "Resubmit", exact: true }).click();
  await until(
    "the corrected receipt lands",
    async () => (await refused.count()) === 0 && (await settled(page)),
    30_000,
  );
  await until("remaining moves", async () => (await remaining(page, "PO-BROWSER-01")) === 3, ONE_POLL);
  await capture(page, "check-5-corrected");
  console.log("CHECK 5 PASS: refused receipt in the not uploaded panel, corrected receipt with the same id landed");

  console.log(`page errors: ${JSON.stringify(errors)}`);
  console.log("PASS: Receiving sync checks");
} catch (error) {
  console.log(`page errors: ${JSON.stringify(errors)}`);
  if (page)
    await page
      .screenshot({ path: join(fixture.reportDirectory, `${step}-failure.png`), fullPage: false })
      .catch((captureError) => console.error(`Failed to capture ${step}:`, captureError));
  throw error;
} finally {
  await browser.close();
}
