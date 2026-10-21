// Use the repository's existing Playwright installation to drive headless Chrome.
import { join } from "node:path";
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
const report = fixture.reportDirectory;
assert.ok(report, "the Rust fixture supplies its report directory");
console.error(`Platform UI screenshots: ${report}`);
const viewport = { width: 1400, height: 900 };
const captured = new Set();
async function capture(page, screen) {
  if (captured.has(screen)) return;
  await page.evaluate(() => document.fonts.ready);
  const root = page.locator("#app > .ui-theme[data-ui]");
  assert.equal(await root.count(), 1, `${screen} has one theme root`);
  assert.equal(await root.locator(":scope > .ui-dashboard").count(), 1, `${screen} has one dashboard shell`);
  assert.equal(await root.getAttribute("data-ui-density"), "compact");
  assert.equal(await root.getAttribute("data-ui-theme"), null);
  assert.ok(await root.locator(".ui-dashboard-panel").count(), `${screen} has a dashboard panel`);
  assert.equal(await root.evaluate((node) => getComputedStyle(node).getPropertyValue("--ui-accent").trim()), "#1971c2");
  await page.screenshot({ path: join(report, `${screen}.png`), fullPage: false });
  captured.add(screen);
}
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
  const page = await browser.newPage({ viewport });
  await page.goto(`${base}/recover`);
  await page.getByRole("button", { name: "send reset link", exact: true }).waitFor();
  await capture(page, "recover");
  await page.goto(`${base}/reset`);
  await page.getByRole("button", { name: "set password", exact: true }).waitFor();
  await capture(page, "reset");
  await page.goto(base);
  await page.getByRole("button", { name: "sign in", exact: true }).waitFor();
  await capture(page, "sign-in");
  await signIn(page, required("WAMN_BROWSER_EMAIL"));
  const audiences = page.getByRole("navigation", { name: "Audiences", exact: true });
  const orgGroup = audiences.locator("details").filter({
    has: page.locator(":scope > summary").getByText(org, { exact: true }),
  });
  const projectGroup = orgGroup.locator("details").filter({
    has: page.locator(":scope > summary").getByText(fixture.releaseProject, { exact: true }),
  });
  const control = orgGroup.getByRole("link", { name: "Control", exact: true });
  await control.waitFor();
  await projectGroup.getByRole("link", { name: fixture.releaseEnv, exact: true }).waitFor();
  assert.equal(await orgGroup.getAttribute("open"), "");
  assert.equal(await projectGroup.getAttribute("open"), "");
  assert.deepEqual(await audiences.getByRole("link").allTextContents(), ["Control", fixture.releaseEnv]);
  assert.equal(await control.getAttribute("href"), controlPath);
  assert.equal(await page.getByLabel("email", { exact: true }).count(), 0);
  assert.equal(await page.getByLabel("password", { exact: true }).count(), 0);
  assert.equal(await page.getByRole("button", { name: "sign in", exact: true }).count(), 0);
  assert.equal(await audiences.getByRole("table").count(), 0);
  assert.equal(await page.locator(".identity-panel").getByRole("navigation", { name: "Audiences" }).count(), 1);
  await capture(page, "audience-choice");
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

  await page.getByRole("button", { name: fixture.releaseProject, exact: true }).waitFor();
  await capture(page, "control-layout");
  await capture(page, "projects");

  await page
    .getByRole("navigation", { name: "Primary navigation" })
    .getByRole("link", { name: "Users", exact: true })
    .click();
  await page.getByRole("heading", { name: "Users", exact: true }).waitFor();
  await page.getByRole("textbox", { name: "Email", exact: true }).fill(fixture.email);
  await page.getByLabel("Display name", { exact: true }).fill("Invited User");
  await page.getByRole("button", { name: "Invite user", exact: true }).click();
  await page.getByText("User invited.", { exact: true }).waitFor();
  const invited = page
    .getByRole("table", { name: "Users", exact: true })
    .getByRole("row")
    .filter({ has: page.getByRole("cell", { name: fixture.email, exact: true }) });
  await invited.getByText("User state: active", { exact: true }).waitFor();
  await capture(page, "users");
  console.log("invitation-created");
  const { code } = JSON.parse((await messages.next()).value);
  fixture.principalId = code.slice(0, code.indexOf(":"));
  const invitedContext = await browser.newContext({ viewport });
  try {
    const enrollment = await invitedContext.newPage();
    enrollment.on("pageerror", (error) => errors.push(error.message));
    await enrollment.goto(`${base}/invite#${code}`);
    await enrollment.getByLabel("email", { exact: true }).waitFor();
    await capture(enrollment, "invite");
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
  assert.equal(
    await page
      .getByRole("table", { name: "Environments", exact: true })
      .locator(":scope > tbody > tr[data-row]")
      .count(),
    0,
  );
  // The Rust fixture provisions this project's environment through platform verbs.
  console.log("project-created");
  assert.equal((await messages.next()).value, "environment-ready");
  await visit(page);
  await page
    .getByRole("table", { name: "Environments", exact: true })
    .getByText(fixture.env, { exact: true })
    .waitFor();
  await capture(page, "project");
  await visit(page, `${base}${controlPath}/users/${fixture.principalId}`);
  await page.getByRole("heading", { name: "Invited User", exact: true }).waitFor();
  await page.getByRole("button").and(page.locator('[aria-label="Project"]')).click();
  await page.getByRole("option", { name: fixture.project, exact: true }).click();
  await page.getByRole("button").and(page.locator('[aria-label="Environment"]')).click();
  await page.getByRole("option", { name: fixture.env, exact: true }).click();
  await page.getByRole("button", { name: "Grant membership", exact: true }).click();
  const access = page
    .getByRole("table", { name: "Project access", exact: true })
    .locator(`:scope > tbody > tr[data-row="${fixture.project}"]`);
  await access.getByRole("button", { name: `Expand details for row ${fixture.project}`, exact: true }).click();
  const userMemberships = page.getByRole("table", { name: `Memberships in ${fixture.project}`, exact: true });
  await userMemberships.getByRole("button", { name: `Revoke membership in ${fixture.env}`, exact: true }).waitFor();
  await capture(page, "user");
  await visit(page);
  await access.getByRole("button", { name: `Expand details for row ${fixture.project}`, exact: true }).click();
  await access.getByRole("button", { name: "Grant project admin", exact: true }).click();
  await access.getByText("Project admin", { exact: true }).waitFor();
  await access.getByRole("button", { name: "Revoke project admin", exact: true }).click();
  await access.getByText("User", { exact: true }).waitFor();
  await userMemberships.getByRole("button", { name: `Revoke membership in ${fixture.env}`, exact: true }).click();
  await userMemberships.locator(":scope > tbody > tr[data-row]").waitFor({ state: "detached" });
  console.log(
    "PASS: Users invite, browser enrollment and sign-in, activate/deactivate, org-admin grant/revoke; User membership and project-admin grant/revoke, reload state",
  );
  await visit(page, `${base}${controlPath}/projects/${fixture.project}`);
  await page.getByRole("textbox", { name: "User ID", exact: true }).fill(fixture.principalId);
  await page.getByRole("button").and(page.locator('[aria-label="Environment"]')).click();
  await page.getByRole("option", { name: fixture.env, exact: true }).click();
  await page.getByRole("button", { name: "Grant membership", exact: true }).click();
  const person = page.locator(`tr[data-row="${fixture.principalId}"]`);
  await person.getByText("Member", { exact: true }).waitFor();
  await person.getByRole("button", { name: `Expand details for row ${fixture.principalId}`, exact: true }).click();
  const memberEnvironments = page.getByRole("table", { name: `Memberships for ${fixture.email}`, exact: true });
  await memberEnvironments.getByRole("button", { name: `Revoke membership in ${fixture.env}`, exact: true }).waitFor();
  await person.getByRole("button", { name: "Grant project admin", exact: true }).click();
  await person.getByText("Project admin", { exact: true }).waitFor();

  const secondContext = await browser.newContext({ viewport });
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
      await second.getByRole("table", { name: "Projects", exact: true }).locator("tbody button").allTextContents(),
      [fixture.project],
    );
    assert.equal(await second.getByRole("button", { name: "Create project", exact: true }).count(), 0);
    await second.getByRole("button", { name: fixture.project, exact: true }).click();
    await second.getByRole("heading", { name: fixture.project, exact: true }).waitFor();
    assert.equal(await second.getByRole("button", { name: /^(Activate|Inactivate) project$/ }).count(), 0);
    await visit(second, `${base}${controlPath}/projects/receiving`);
    await second.getByText("This address names no page.", { exact: true }).waitFor();
    await capture(second, "not-found");
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
  await memberEnvironments.getByRole("button", { name: `Revoke membership in ${fixture.env}`, exact: true }).click();
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
    .getByRole("table", { name: "Environments", exact: true })
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
  assert.ok(
    (await page
      .getByRole("table", { name: "Operations", exact: true })
      .locator(":scope > tbody > tr[data-row]")
      .count()) > 0,
  );
  await capture(page, "environment");
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
  const receiving = await browser.newPage({ viewport });
  await signIn(receiving, required("WAMN_BROWSER_EMAIL"));
  const receivingOrg = receiving
    .getByRole("navigation", { name: "Audiences" })
    .locator("details")
    .filter({
      has: receiving.locator(":scope > summary").getByText(org, { exact: true }),
    });
  const receivingProject = receivingOrg.locator("details").filter({
    has: receiving.locator(":scope > summary").getByText(fixture.releaseProject, { exact: true }),
  });
  const environment = receivingProject.getByRole("link", { name: fixture.releaseEnv, exact: true });
  await environment.waitFor();
  const environmentPath = await environment.getAttribute("href");
  assert.ok(
    environmentPath.startsWith(`/urn:wamn:project-env:${org}:${fixture.releaseProject}:${fixture.releaseEnv}:`),
  );
  await environment.click();
  await receiving.getByRole("button", { name: "sign out", exact: true }).waitFor();
  assert.equal(new URL(receiving.url()).pathname, environmentPath);
  await capture(receiving, "receiving-layout");
  await receiving.close();
  console.log(
    "PASS: audience hierarchy replaces sign-in; Control first; org/project/environment entries enter their audiences",
  );
  assert.deepEqual(
    [...captured].sort(),
    [
      "sign-in",
      "invite",
      "recover",
      "reset",
      "audience-choice",
      "control-layout",
      "projects",
      "project",
      "environment",
      "users",
      "user",
      "receiving-layout",
      "not-found",
    ].sort(),
  );
  assert.deepEqual(errors, []);
  console.error(
    `Platform UI PNGs: ${[...captured]
      .sort()
      .map((screen) => `${screen}.png`)
      .join(", ")}`,
  );
} finally {
  input.close();
  await browser.close();
}
