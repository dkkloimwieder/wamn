// Use the repository's existing Playwright installation to drive headless Chrome.
import { createRequire } from "node:module";
import assert from "node:assert/strict";

const require = createRequire(new URL("../../../web/shell/package.json", import.meta.url));
const { chromium } = require("playwright");
const required = (name) => {
  const value = process.env[name];
  assert.ok(value, `set ${name}`);
  return value;
};
const org = required("WAMN_BROWSER_ORG");
const browser = await chromium.launch({ channel: "chrome", headless: true });
try {
  const page = await browser.newPage();
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await page.goto(required("WAMN_BROWSER_URL"));
  await page.getByLabel("email", { exact: true }).fill(required("WAMN_BROWSER_EMAIL"));
  await page.getByLabel("password", { exact: true }).fill(required("WAMN_BROWSER_PASSWORD"));
  await page.getByRole("button", { name: "sign in", exact: true }).click();
  const control = page.getByRole("button", { name: "Control", exact: true });
  await control.waitFor();
  await page.getByRole("button", { name: `${org}/receiving/dev`, exact: true }).waitFor();
  const mine = page.waitForResponse((response) => response.url().endsWith("/api/wamn_control/control/mine"));
  await control.click();
  const response = await mine;
  assert.equal(response.status(), 200);
  assert.equal((await response.json())[0].value.org_admin, true);
  await page.getByRole("button", { name: "sign out", exact: true }).waitFor();
  await page.getByText(org, { exact: true }).waitFor();
  assert.equal(new URL(page.url()).pathname, `/urn:wamn:control:${org}`);
  assert.equal(await page.getByRole("navigation", { name: "Primary navigation" }).count(), 1);
  assert.equal(await page.locator("nav section").count(), 0);
  assert.equal((await page.locator("main").textContent()).trim(), "");
  assert.equal(await page.getByText("This address names no page.", { exact: true }).count(), 0);
  assert.equal(await page.locator("#app > [data-ui]").count(), 1);
  assert.equal(await page.locator("#app > [data-ui]").getAttribute("data-ui-density"), "compact");
  assert.deepEqual(errors, []);
  console.log("PASS: sign-in -> Control and receiving/dev -> empty org layout with data-ui");
} finally {
  await browser.close();
}
