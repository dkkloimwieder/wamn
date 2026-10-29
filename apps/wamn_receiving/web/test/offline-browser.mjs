// The five offline profile checks of the open receipt lines screen. The Rust fixture starts the real local services.
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
console.error(`Receiving offline screenshots: ${fixture.reportDirectory}`);

const OPEN = "Open receipt lines";
const LOCATION_ID = "00000000-0000-0000-0000-000000000201";
// The collection polls every 10 s. One poll and the time the write takes to land.
const ONE_POLL = 13_000;
// WAMN_OFFLINE_CHECK=<1-5> runs one check alone. Each check has its own browser context and orders.
const only = process.env.WAMN_OFFLINE_CHECK;
const runs = (check) => !only || only === String(check);
const browser = await chromium.launch({ channel: "chrome", headless: true });
const errors = [];
let page;
let step = "start";

const table = (on) => on.getByRole("table", { name: OPEN, exact: true });
const statusLine = (on) => on.getByLabel("Sync status", { exact: true });
const notUploaded = (on) => on.getByRole("region", { name: "Not uploaded", exact: true }).locator("article");
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
const status = async (on) => (await statusLine(on).textContent()) ?? "";

/** Sign in, open the open receipt lines screen, and wait until the worker controls the page. */
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
  await on.evaluate(() => navigator.serviceWorker.ready);
  // The worker controls the page from its next load; the reopen needs it.
  await on.reload();
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

/** The saved user record of the page's application environment, and its storage key. */
const savedUser = (on) =>
  on.evaluate(() => {
    const key = Object.keys(localStorage).find((name) => name.startsWith("wamn-user:"));
    return key === undefined ? undefined : { key, user: JSON.parse(localStorage.getItem(key)) };
  });

/** Replace the settings saved with the user, as a settings read while online would. */
const saveSettings = (on, settings) =>
  on.evaluate((settings) => {
    const key = Object.keys(localStorage).find((name) => name.startsWith("wamn-user:"));
    localStorage.setItem(key, JSON.stringify({ ...JSON.parse(localStorage.getItem(key)), settings }));
  }, settings);

/** Whether the user's rows database holds a saved collection, which an offline reopen paints. */
const rowsSaved = (on) =>
  on.evaluate(async () => {
    const name = (await indexedDB.databases()).map((entry) => entry.name).find((n) => n?.startsWith("wamn-rows:"));
    if (name === undefined) return false;
    const db = await new Promise((resolve, reject) => {
      const request = indexedDB.open(name);
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => reject(request.error);
    });
    try {
      if (!db.objectStoreNames.contains("rows")) return false;
      return await new Promise((resolve, reject) => {
        const request = db.transaction("rows").objectStore("rows").count();
        request.onsuccess = () => resolve(request.result > 0);
        request.onerror = () => reject(request.error);
      });
    } finally {
      db.close();
    }
  });

/**
 * A signed-in page in a fresh context, so no check sees another check's
 * storage or clock. It is tall enough that every row renders below the
 * panels, and it waits until the rows are saved.
 */
async function freshPage() {
  const context = await browser.newContext({ viewport: { width: 1400, height: 2400 } });
  page = await context.newPage();
  watch(page);
  await signIn(page);
  await until("the rows are saved", () => rowsSaved(page), 30_000);
  return context;
}

/** Close the page and open the same address again in the same context. */
async function reopen(context) {
  const address = page.url();
  await page.close();
  page = await context.newPage();
  watch(page);
  await page.goto(address);
}

try {
  if (runs(1)) {
    step = "check-1";
    const context = await freshPage();
    const { user } = await savedUser(page);
    assert.ok(user.loginExpiresAt > Date.now() / 1000, "the saved login has not expired yet");
    await context.setOffline(true);
    // The device clock moves past the saved login expiry.
    await context.clock.setFixedTime(user.loginExpiresAt * 1000 + 60_000);
    await reopen(context);
    await page.getByRole("button", { name: "Sign in", exact: true }).waitFor();
    assert.equal(await table(page).count(), 0, "no rows paint after the login expired");
    await capture(page, "check-1-sign-in");
    console.log("CHECK 1 PASS: session reopen after loginExpiresAt shows the sign-in form");
    await context.close();
  }

  if (runs(2)) {
    step = "check-2";
    const context = await freshPage();
    const before = await remaining(page, "PO-BROWSER-06");
    await context.setOffline(true);
    await reopen(context);
    await row(page, "PO-BROWSER-06").waitFor();
    assert.equal(await table(page).locator("tbody > tr[data-row]").count(), 25, "saved rows paint");
    assert.match(await status(page), /Offline, sign in to upload\./);
    await record(page, "PO-BROWSER-06", "OFF-SESSION", "1");
    assert.ok(await tentative(page, "PO-BROWSER-06"));
    assert.equal(await remaining(page, "PO-BROWSER-06"), before - 1);
    assert.match(await status(page), /Pending: 1\./);
    await capture(page, "check-2-reopened");
    await context.setOffline(false);
    await until(
      "the receipt lands",
      async () => !(await tentative(page, "PO-BROWSER-06")) && /Pending: 0\./.test(await status(page)),
      30_000,
    );
    console.log("CHECK 2 PASS: session reopen before loginExpiresAt painted the saved rows and recorded");
    await context.close();
  }

  if (runs(3)) {
    step = "check-3";
    const context = await freshPage();
    const sent = [];
    context.on("request", (request) => {
      if (new URL(request.url()).pathname.endsWith("/receiving/record_receipt")) sent.push(request.url());
    });
    // A short age stands in for an hour, so the entries expire without moving the clock.
    await saveSettings(page, { profile: "session", pendingMaxAge: "PT5S", clearOnSignOut: true });
    await reopen(context);
    await row(page, "PO-BROWSER-07").waitFor();
    const kept = await remaining(page, "PO-BROWSER-07");
    const dropped = await remaining(page, "PO-BROWSER-08");
    await context.setOffline(true);
    await record(page, "PO-BROWSER-07", "OFF-EXPIRE-KEEP", "1");
    await record(page, "PO-BROWSER-08", "OFF-EXPIRE-DROP", "1");
    await new Promise((resolve) => setTimeout(resolve, 6_000));
    await context.setOffline(false);
    await reopen(context);
    await until("both entries expire", async () => /Expired: 2\./.test(await status(page)), 30_000);
    const expired = notUploaded(page).filter({ hasText: "Expired." });
    assert.equal(await expired.count(), 2);
    assert.match((await expired.first().textContent()) ?? "", /Recorded at \d{4}-\d\d-\d\dT/);
    await new Promise((resolve) => setTimeout(resolve, ONE_POLL));
    assert.deepEqual(sent, [], "no expired entry is sent");
    assert.ok(await tentative(page, "PO-BROWSER-07"), "an expired entry keeps its tentative row");
    await capture(page, "check-3-expired");
    // The entries list in recorded order, so the first is the receipt for PO-BROWSER-07.
    await expired.first().getByRole("button", { name: "Resubmit", exact: true }).click();
    await until(
      "the resubmitted receipt lands",
      async () => /Pending: 0\. Refused: 0\. Expired: 1\./.test(await status(page)),
      30_000,
    );
    await until("remaining moves once", async () => (await remaining(page, "PO-BROWSER-07")) === kept - 1, ONE_POLL);
    assert.equal(sent.length, 1, "the resubmitted entry is sent once");
    await notUploaded(page).getByRole("button", { name: "Discard", exact: true }).click();
    await until("the discarded entry goes", async () => (await notUploaded(page).count()) === 0, 10_000);
    await until(
      "the tentative row goes",
      async () => !(await tentative(page, "PO-BROWSER-08")) && (await remaining(page, "PO-BROWSER-08")) === dropped,
      10_000,
    );
    assert.equal(sent.length, 1, "the discarded entry is never sent");
    // Both restored entries write one overlay store, so the landed row stays tentative until the other is resolved.
    await until(
      "the resubmitted row is no longer tentative",
      async () => !(await tentative(page, "PO-BROWSER-07")),
      // Confirmation needs a feed pass that starts after the send, up to two polls.
      30_000,
    );
    await capture(page, "check-3-cleared");
    console.log("CHECK 3 PASS: expired entries not sent, resubmit sent once, discard removed the row");
    await context.close();
  }

  if (runs(4)) {
    step = "check-4";
    const context = await freshPage();
    const { key, user } = await savedUser(page);
    const [, application, environment] = key.split(":");
    const outboxName = `wamn-outbox:${application}:${environment}:${user.userId}`;
    const rowsName = `wamn-rows:${application}:${environment}:${user.userId}`;
    await context.setOffline(true);
    await record(page, "PO-BROWSER-09", "OFF-SIGN-OUT", "1");
    assert.match(await status(page), /Pending: 1\./);
    const databases = () => page.evaluate(async () => (await indexedDB.databases()).map((entry) => entry.name));
    assert.ok((await databases()).includes(outboxName), "the outbox is saved before sign-out");
    assert.ok((await databases()).includes(rowsName), "the rows are saved before sign-out");
    await context.setOffline(false);
    await page.getByRole("button", { name: "User menu", exact: true }).click();
    await page.getByRole("menuitem", { name: "Sign out", exact: true }).click();
    await page.getByRole("button", { name: "Sign in", exact: true }).waitFor();
    await until(
      "sign-out deletes the rows and the outbox",
      async () => {
        const names = await databases();
        return !names.includes(outboxName) && !names.includes(rowsName);
      },
      10_000,
    );
    assert.equal(await page.evaluate((key) => localStorage.getItem(key), key), null, "the saved user goes");
    await capture(page, "check-4-signed-out");
    console.log("CHECK 4 PASS: sign-out with clear_on_sign_out deleted the saved user, rows and outbox");
    await context.close();
  }

  if (runs(5)) {
    step = "check-5";
    const context = await freshPage();
    const before = await remaining(page, "PO-BROWSER-10");
    await saveSettings(page, { profile: "local_first", clearOnSignOut: false });
    const { user } = await savedUser(page);
    await context.setOffline(true);
    // No session: the device clock moves past the saved login expiry.
    await context.clock.setFixedTime(user.loginExpiresAt * 1000 + 60_000);
    await reopen(context);
    await row(page, "PO-BROWSER-10").waitFor();
    assert.equal(await table(page).locator("tbody > tr[data-row]").count(), 25, "saved rows paint");
    await record(page, "PO-BROWSER-10", "OFF-LOCAL-FIRST", "1");
    assert.ok(await tentative(page, "PO-BROWSER-10"));
    assert.equal(await remaining(page, "PO-BROWSER-10"), before - 1);
    assert.match(await status(page), /Pending: 1\./);
    await capture(page, "check-5-local-first");
    console.log("CHECK 5 PASS: local_first reopened and recorded with no session");
    await context.close();
  }

  console.log(`page errors: ${JSON.stringify(errors)}`);
  assert.deepEqual(errors, []);
  console.log("PASS: Receiving offline profile checks");
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
