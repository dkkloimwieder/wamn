// Use the repository's existing Playwright installation to drive headless Chrome.
import { createRequire } from "node:module";
import { createInterface } from "node:readline";
import assert from "node:assert/strict";
import { setTimeout } from "node:timers/promises";

const require = createRequire(new URL("../../../web/shell/package.json", import.meta.url));
const { chromium } = require("playwright");
const required = (name) => {
  const value = process.env[name];
  assert.ok(value, `set ${name}`);
  return value;
};
const input = createInterface({ input: process.stdin });
const messages = input[Symbol.asyncIterator]();
const fixture = JSON.parse((await messages.next()).value);
const org = required("WAMN_BROWSER_ORG");
const base = required("WAMN_BROWSER_URL");
const controlPath = `/urn:wamn:control:${org}`;
const browser = await chromium.launch({ channel: "chrome", headless: true });
const errors = [];
async function signIn(page, email, address = base) {
  page.on("pageerror", (error) => errors.push(error.message));
  await page.goto(address);
  await page.getByLabel("email", { exact: true }).fill(email);
  await page.getByLabel("password", { exact: true }).fill(required("WAMN_BROWSER_PASSWORD"));
  await page.getByRole("button", { name: "sign in", exact: true }).click();
}
// Address and reload assertions renew the session. Respect identity's Retry-After
// response when the combined browser journeys fill its source window.
async function visit(page, address = page.url()) {
  for (;;) {
    const renewed = page.waitForResponse((response) => new URL(response.url()).pathname === "/password/renew");
    await page.goto(address);
    const response = await renewed;
    if (response.status() !== 429) {
      assert.equal(response.status(), 200, `Session renewal at ${new URL(address).pathname}`);
      return;
    }
    const seconds = Number(response.headers()["retry-after"]);
    assert.ok(Number.isFinite(seconds) && seconds > 0, "Identity must state Retry-After");
    console.error(`Identity answered 429; retrying the address after ${seconds}s`);
    await setTimeout(seconds * 1000);
  }
}
try {
  const page = await browser.newPage();
  await signIn(page, required("WAMN_BROWSER_EMAIL"));
  const control = page.getByRole("button", { name: "Control", exact: true });
  await control.waitFor();
  await page.getByRole("button", { name: `${org}/receiving/dev`, exact: true }).waitFor();
  const mine = page.waitForResponse((response) => response.url().endsWith("/api/wamn_control/control/mine"));
  await control.click();
  const response = await mine;
  assert.equal(response.status(), 200);
  assert.equal((await response.json())[0].value.org_admin, true);
  await page.getByRole("heading", { name: "Projects", exact: true }).waitFor();
  assert.equal(new URL(page.url()).pathname, `${controlPath}/projects`);
  assert.equal(await page.getByRole("navigation", { name: "Primary navigation" }).count(), 1);
  assert.equal(await page.locator("#app > [data-ui]").count(), 1);
  assert.equal(await page.locator("#app > [data-ui]").getAttribute("data-ui-density"), "compact");

  await page
    .getByRole("navigation", { name: "Primary navigation" })
    .getByRole("link", { name: "Users", exact: true })
    .click();
  await page.getByRole("heading", { name: "Users", exact: true }).waitFor();
  await page.getByLabel("Email", { exact: true }).fill(fixture.email);
  await page.getByLabel("Display name", { exact: true }).fill("Invited User");
  await page.getByRole("button", { name: "Invite user", exact: true }).click();
  await page.getByText("User invited.", { exact: true }).waitFor();
  const invited = page.getByRole("listitem", { name: fixture.email, exact: true });
  await invited.getByText("User state: active", { exact: true }).waitFor();
  console.log("invitation-created");
  const { code } = JSON.parse((await messages.next()).value);
  fixture.principalId = code.slice(0, code.indexOf(":"));
  const invitedContext = await browser.newContext();
  try {
    const enrollment = await invitedContext.newPage();
    enrollment.on("pageerror", (error) => errors.push(error.message));
    await enrollment.goto(`${base}/invite#${code}`);
    await enrollment.getByLabel("email", { exact: true }).fill(fixture.email);
    await enrollment.getByLabel("password", { exact: true }).fill(required("WAMN_BROWSER_PASSWORD"));
    await enrollment.getByLabel("password again", { exact: true }).fill(required("WAMN_BROWSER_PASSWORD"));
    await enrollment.getByRole("button", { name: "set password", exact: true }).click();
    await enrollment.getByRole("button", { name: "sign in", exact: true }).waitFor();
    assert.equal(new URL(enrollment.url()).hash, "");
    await invited.getByRole("button", { name: "Deactivate user", exact: true }).click();
    await invited.getByText("User state: inactive", { exact: true }).waitFor();
    assert.equal(await invited.getByRole("button", { name: "Deactivate user", exact: true }).count(), 0);
    await visit(page);
    await invited.getByRole("button", { name: "Activate user", exact: true }).click();
    await invited.getByText("User state: active", { exact: true }).waitFor();
    assert.equal(await invited.getByRole("button", { name: "Activate user", exact: true }).count(), 0);
    await invited.getByRole("button", { name: "Grant org admin", exact: true }).click();
    await invited.getByText("Org admin", { exact: true }).waitFor();
    assert.equal(await invited.getByRole("button", { name: "Grant org admin", exact: true }).count(), 0);
    await signIn(enrollment, fixture.email, `${base}${controlPath}`);
    await enrollment.getByRole("heading", { name: "Projects", exact: true }).waitFor();
    await visit(page);
    await invited.getByRole("button", { name: "Revoke org admin", exact: true }).click();
    await invited.getByText("User", { exact: true }).waitFor();
    assert.equal(await invited.getByRole("button", { name: "Revoke org admin", exact: true }).count(), 0);
    await visit(page);
    await invited.getByRole("button", { name: "Grant org admin", exact: true }).waitFor();
  } finally {
    await invitedContext.close();
  }
  await visit(page, `${base}${controlPath}/projects`);
  await page.getByLabel("Project name", { exact: true }).fill(fixture.project);
  await page.getByRole("button", { name: "Create project", exact: true }).click();
  await page.getByText("Project created.", { exact: true }).waitFor();
  await page.getByRole("button", { name: fixture.project, exact: true }).click();
  await page.getByRole("heading", { name: fixture.project, exact: true }).waitFor();
  assert.equal(new URL(page.url()).pathname, `${controlPath}/projects/${fixture.project}`);
  assert.equal(await page.getByRole("list", { name: "Environments", exact: true }).getByRole("listitem").count(), 0);
  // The Rust fixture provisions this project's environment through platform verbs.
  console.log("project-created");
  assert.equal((await messages.next()).value, "environment-ready");
  await visit(page);
  await page.getByRole("list", { name: "Environments", exact: true }).getByText(fixture.env, { exact: true }).waitFor();
  await visit(page, `${base}${controlPath}/users/${fixture.principalId}`);
  await page.getByRole("heading", { name: "Invited User", exact: true }).waitFor();
  await page.getByRole("button").and(page.locator('[aria-label="Project"]')).click();
  await page.getByRole("option", { name: fixture.project, exact: true }).click();
  await page.getByRole("button").and(page.locator('[aria-label="Environment"]')).click();
  await page.getByRole("option", { name: fixture.env, exact: true }).click();
  await page.getByRole("button", { name: "Grant membership", exact: true }).click();
  const access = page.getByRole("listitem", { name: fixture.project, exact: true });
  await access.getByRole("button", { name: `Revoke membership in ${fixture.env}`, exact: true }).waitFor();
  await visit(page);
  await access.getByRole("button", { name: "Grant project admin", exact: true }).click();
  await access.getByText("Project admin", { exact: true }).waitFor();
  await access.getByRole("button", { name: "Revoke project admin", exact: true }).click();
  await access.getByText("User", { exact: true }).waitFor();
  await access.getByRole("button", { name: `Revoke membership in ${fixture.env}`, exact: true }).click();
  await access
    .getByRole("list", { name: `Memberships in ${fixture.project}`, exact: true })
    .getByRole("listitem")
    .waitFor({ state: "detached" });
  console.log(
    "PASS: Users invite, browser enrollment and sign-in, activate/deactivate, org-admin grant/revoke; User membership and project-admin grant/revoke, reload state",
  );
  await visit(page, `${base}${controlPath}/projects/${fixture.project}`);
  await page.getByLabel("User ID", { exact: true }).fill(fixture.principalId);
  await page.getByRole("button").and(page.locator('[aria-label="Environment"]')).click();
  await page.getByRole("option", { name: fixture.env, exact: true }).click();
  await page.getByRole("button", { name: "Grant membership", exact: true }).click();
  const person = page.getByRole("listitem", { name: fixture.email, exact: true });
  await person.getByText("Member", { exact: true }).waitFor();
  await person.getByRole("button", { name: `Revoke membership in ${fixture.env}`, exact: true }).waitFor();
  await person.getByRole("button", { name: "Grant project admin", exact: true }).click();
  await person.getByText("Project admin", { exact: true }).waitFor();

  const secondContext = await browser.newContext();
  const projectAdminStarted = performance.now();
  try {
    const second = await secondContext.newPage();
    const projectListRequests = [];
    second.on("request", (request) => {
      if (new URL(request.url()).pathname === "/api/wamn_control/project/list") projectListRequests.push(request.url());
    });
    await signIn(second, fixture.email, `${base}${controlPath}`);
    await second.getByRole("heading", { name: "Projects", exact: true }).waitFor();
    await second.getByRole("button", { name: fixture.project, exact: true }).waitFor();
    assert.deepEqual(
      await second.getByRole("list", { name: "Projects", exact: true }).getByRole("button").allTextContents(),
      [fixture.project],
    );
    assert.equal(await second.getByRole("button", { name: "Create project", exact: true }).count(), 0);
    await second.getByRole("button", { name: fixture.project, exact: true }).click();
    await second.getByRole("heading", { name: fixture.project, exact: true }).waitFor();
    assert.equal(await second.getByRole("button", { name: /^(Activate|Inactivate) project$/ }).count(), 0);
    await visit(second, `${base}${controlPath}/projects/receiving`);
    await second.getByText("This address names no page.", { exact: true }).waitFor();
    await visit(second, `${base}${controlPath}/users`);
    await second.getByText("This address names no page.", { exact: true }).waitFor();
    await visit(second, `${base}${controlPath}/users/${fixture.principalId}`);
    await second.getByText("This address names no page.", { exact: true }).waitFor();
    assert.deepEqual(projectListRequests, []);
    assert.deepEqual(errors, []);
    console.log(
      `PASS: project-admin lands on Projects, sees only its project, calls no project.list, and cannot open receiving (${((performance.now() - projectAdminStarted) / 1000).toFixed(2)}s)`,
    );
  } finally {
    await secondContext.close();
  }

  await person.getByRole("button", { name: "Revoke project admin", exact: true }).click();
  await person.getByText("Member", { exact: true }).waitFor();
  await person.getByRole("button", { name: `Revoke membership in ${fixture.env}`, exact: true }).click();
  await person.waitFor({ state: "detached" });
  await page.getByRole("button", { name: "Grant membership", exact: true }).click();
  await person.getByText("Member", { exact: true }).waitFor();

  await page.getByRole("button", { name: "Inactivate project", exact: true }).click();
  await page.getByRole("button", { name: "Activate project", exact: true }).waitFor();
  assert.equal(await page.getByRole("button", { name: "Inactivate project", exact: true }).count(), 0);
  await visit(page);
  await page.getByRole("button", { name: "Activate project", exact: true }).click();
  await page.getByRole("button", { name: "Inactivate project", exact: true }).waitFor();
  assert.equal(await page.getByRole("button", { name: "Activate project", exact: true }).count(), 0);
  await visit(page);
  await page.getByRole("button", { name: "Inactivate project", exact: true }).waitFor();
  await visit(page, `${base}${controlPath}/projects/${fixture.releaseProject}`);
  await page.getByRole("heading", { name: fixture.releaseProject, exact: true }).waitFor();
  await page
    .getByRole("list", { name: "Environments", exact: true })
    .getByRole("button", { name: fixture.releaseEnv, exact: true })
    .click();
  assert.equal(new URL(page.url()).pathname, `${controlPath}/projects/${fixture.releaseProject}/${fixture.releaseEnv}`);
  await page.getByRole("heading", { name: `${fixture.releaseProject}/${fixture.releaseEnv}`, exact: true }).waitFor();
  await page
    .getByRole("region", { name: "Environment status" })
    .getByText(`Release: ${fixture.releaseDigest}`, { exact: true })
    .waitFor();
  await page
    .getByRole("region", { name: "Release profile" })
    .getByText(fixture.releaseDigest, { exact: true })
    .waitFor();
  assert.ok((await page.getByRole("list", { name: "Operations", exact: true }).getByRole("listitem").count()) > 0);
  await page.getByRole("button", { name: "Inactivate environment", exact: true }).click();
  await page.getByText("Environment state: inactive", { exact: true }).waitFor();
  assert.equal(await page.getByRole("button", { name: "Inactivate environment", exact: true }).count(), 0);
  await visit(page);
  await page.getByRole("button", { name: "Activate environment", exact: true }).click();
  await page.getByText("Environment state: active", { exact: true }).waitFor();
  assert.equal(await page.getByRole("button", { name: "Activate environment", exact: true }).count(), 0);
  await visit(page);
  await page.getByRole("button", { name: "Inactivate environment", exact: true }).waitFor();
  await page.getByRole("region", { name: "Release profile" }).waitFor();
  assert.deepEqual(errors, []);
  console.log(
    "PASS: Environment navigation, local runtime status, release profile, single activate/inactivate action and reload state",
  );
  console.log(
    "PASS: Control lands on Projects; create, membership grant/revoke, project-admin grant/revoke, scoped access, activate/inactivate and reload state",
  );
} finally {
  input.close();
  await browser.close();
}
