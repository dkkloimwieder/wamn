/**
 * The shell in Chrome, through an application host and a control host
 * (docs/plan/platform-ui.md §4.8, wamn-a40n.11).
 *
 * `tests/integration/src/shell_browser_live.rs` serves the hosts, the
 * identity stand-in and the page, and runs this file with the page address in
 * `WAMN_BROWSER_URL` and the application audience in `WAMN_BROWSER_AUDIENCE`.
 * Each persona signs in on its own browser context. A failed step throws, and
 * the process exits with a nonzero status.
 */

import { chromium } from "playwright";

const BASE = process.env.WAMN_BROWSER_URL;
const AUDIENCE = process.env.WAMN_BROWSER_AUDIENCE;
if (!BASE || !AUDIENCE) {
  throw new Error("set WAMN_BROWSER_URL and WAMN_BROWSER_AUDIENCE");
}
const WAIT = { timeout: 15000 };

const browser = await chromium.launch({ channel: "chrome", headless: true });

/** Signs `email` in at the root on a new context, and returns its page. */
async function signIn(email) {
  const context = await browser.newContext();
  const page = await context.newPage();
  page.on("pageerror", (error) => console.error(`${email}: page error: ${error.message}`));
  page.on("console", (message) => console.error(`${email}: console: ${message.text()}`));
  page.on("response", (response) => {
    if (response.status() >= 400) {
      console.error(`${email}: ${response.status()} ${response.request().method()} ${response.url()}`);
    }
  });
  await page.goto(`${BASE}/`);
  await page.getByLabel("email").fill(email);
  await page.getByLabel("password").fill("a browser test password");
  await page.getByRole("button", { name: "sign in" }).click();
  return page;
}

async function visible(page, text) {
  await page.getByText(text, { exact: true }).first().waitFor(WAIT);
}

async function absent(page, text) {
  const count = await page.getByText(text, { exact: true }).count();
  if (count !== 0) {
    throw new Error(`"${text}" shows, and it must not`);
  }
}

async function step(name, run) {
  try {
    await run();
  } catch (error) {
    for (const page of browser.contexts().flatMap((context) => context.pages())) {
      console.error(`page ${page.url()}:\n${await page.locator("body").innerText()}`);
    }
    throw error;
  }
  console.log(`ok: ${name}`);
}

try {
  await step("an account with no audience sees only that no access has been granted", async () => {
    const page = await signIn("nobody@example.test");
    await visible(page, "No access has been granted.");
    await absent(page, "Browser test");
  });

  await step("a member sees only the screen of the operation it holds", async () => {
    const page = await signIn("member@example.test");
    await visible(page, "purchase read screen");
    await absent(page, "write");
    await absent(page, "Administration");
    await page.goto(`${BASE}/${AUDIENCE}/purchases/new`);
    await visible(page, "This address names no page.");
  });

  await step("an admin sees every screen and the Administration grids", async () => {
    const page = await signIn("first@example.test");
    await visible(page, "purchase read screen");
    await visible(page, "write");
    await page.getByText("roles", { exact: true }).click();
    await page.getByRole("button", { name: "role" }).click();
    await page.getByRole("option", { name: "purchase-reader" }).waitFor(WAIT);
  });

  await step("an org-admin enters Control and sees the org and its projects", async () => {
    const page = await signIn("boss@example.test");
    await visible(page, "Org administration.");
    await page.getByText("billing", { exact: true }).click();
    await visible(page, "Project administration of billing.");
  });

  await step("a project-admin sees only the project it administers", async () => {
    const page = await signIn("cat@example.test");
    await visible(page, "Project administration of billing.");
    await absent(page, "org");
  });
} finally {
  await browser.close();
}
