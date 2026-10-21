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
  await page.mouse.move(0, 0);
  const root = page.locator("#app > .ui-theme[data-ui]");
  assert.equal(await root.count(), 1, `${screen} has one theme root`);
  if (["sign-in", "invite", "recover", "reset", "no-access", "not-found"].includes(screen)) {
    assert.equal(await page.locator(".ui-dashboard").count(), 0);
    assert.equal(await root.locator(":scope > main > .ui-entry-panel").count(), 1);
    assert.equal(await root.locator(".ui-entry-panel").getAttribute("data-size"), "viewport");
    assert.equal(await root.locator("header, nav").count(), 0);
    const titles = {
      "sign-in": "Sign in",
      invite: "Accept invitation",
      recover: "Recover password",
      reset: "Reset password",
      "no-access": "No access",
      "not-found": "No page here",
    };
    assert.equal(await root.getByRole("heading", { name: titles[screen], exact: true }).count(), 1);
    if (screen === "not-found") {
      assert.equal(await root.locator("form").count(), 0);
      assert.equal(await root.getByRole("heading", { name: "No page here", exact: true }).count(), 1);
      assert.equal(await root.getByRole("link").count(), 1);
      assert.equal(await root.getByRole("link", { name: "Go to audience home" }).getAttribute("href"), controlPath);
    }
  } else {
    assert.equal(await root.locator(":scope > .ui-dashboard").count(), 1, `${screen} has one dashboard shell`);
    // Audience choice and applications without screens have no navigation.
    if (!["audience-choice", "receiving-layout"].includes(screen)) {
      const navigation = root.getByRole("navigation", { name: "Primary navigation" });
      assert.equal(await navigation.count(), 1, `${screen} has one navigation pane`);
      assert.equal(
        await navigation.locator('[aria-current="page"], [data-ui-active="true"], [aria-pressed="true"]').count(),
        1,
        `${screen} has exactly one selected navigation item`,
      );
    }
  }
  assert.equal(await root.getAttribute("data-ui-density"), "compact");
  assert.equal(await root.getAttribute("data-ui-theme"), null);
  assert.ok(
    await root.locator(".ui-dashboard-panel, .platform-table-screen, .ui-entry-panel").count(),
    `${screen} has a dashboard panel, entry panel, or a full-panel table`,
  );
  assert.equal(await root.evaluate((node) => getComputedStyle(node).getPropertyValue("--ui-accent").trim()), "#1971c2");
  if (["sign-in", "invite", "recover", "reset", "not-found"].includes(screen)) {
    assert.equal(await root.locator(".ui-entry-panel").count(), 1);
    assert.equal(await page.getByRole("navigation", { name: "Platform location" }).count(), 0);
  }
  if (
    [
      "create-project",
      "invite-user",
      "project-grant-membership",
      "member-grant-membership",
      "user-grant-membership",
    ].includes(screen)
  ) {
    const form = root.locator("form.ui-form");
    await form.getByRole("button", { name: "Cancel", exact: true }).waitFor();
    assert.equal(await form.locator("button[type=submit]").count(), 1);
  }
  const breadcrumbs = page.getByRole("navigation", { name: "Platform location" });
  for (const text of await breadcrumbs.locator("a, [aria-current=page]").allTextContents())
    assert.equal(text, text.toUpperCase());
  assert.ok(
    await page.getByRole("table").evaluateAll((tables) => tables.filter((table) => !table.closest("td")).length <= 1),
    `${screen} has at most one parent table`,
  );
  for (const group of await root.getByRole("group", { name: "Record actions" }).all()) {
    assert.equal(await group.locator("..").getByRole("heading", { level: 1 }).count(), 1);
    for (const button of await group.getByRole("button").all()) {
      assert.ok(
        (await button.boundingBox()).width < (await root.locator(".ui-dashboard-main").boundingBox()).width / 2,
      );
    }
  }
  for (const table of await root.locator(".platform-table-screen").all()) {
    assert.equal(
      await table.evaluate((node) => {
        const main = node.closest(".ui-dashboard-main");
        const style = getComputedStyle(main);
        return (
          [style.paddingTop, style.paddingRight, style.paddingBottom, style.paddingLeft].every(
            (value) => value === "0px",
          ) && Math.abs(node.getBoundingClientRect().left - main.getBoundingClientRect().left) < 1
        );
      }),
      true,
      `${screen} table touches the main edge without main padding`,
    );
    const pages = table.getByRole("navigation", { name: "Table pages" });
    assert.ok((await pages.count()) >= 1, `${screen} uses wfe pagination`);
    assert.ok(
      await table
        .getByRole("table")
        .evaluateAll((tables) =>
          tables.every((table) => table.querySelectorAll(":scope > tbody > tr[data-row]").length <= 25),
        ),
    );
    await pages.first().getByRole("button", { name: "Previous page", exact: true }).waitFor();
    await pages.first().getByRole("button", { name: "Next page", exact: true }).waitFor();
  }
  await page.screenshot({ path: join(report, `${screen}.png`), fullPage: false });
  captured.add(screen);
}
async function expandReference(table, id) {
  const toggle = table.getByRole("button", { name: `Expand details for row ${id}`, exact: true });
  const workspace = table.locator(
    "xpath=ancestor::section[contains(concat(' ', normalize-space(@class), ' '), ' table-workspace ')][1]",
  );
  const next = workspace
    .locator(":scope > .table-workspace-results > footer")
    .getByRole("button", { name: "Next page", exact: true });
  while (!(await toggle.count())) {
    assert.ok(await next.isEnabled(), `reference row ${id} exists on a remaining page`);
    await next.click();
  }
  await toggle.click();
}
async function expandGroup(page, label) {
  const summary = page.locator("summary").filter({ hasText: label });
  if (!(await summary.evaluate((node) => node.parentElement.open))) await summary.click();
}
const org = required("WAMN_BROWSER_ORG");
const base = required("WAMN_BROWSER_URL");
const controlPath = `/urn:wamn:control:${org}`;
const browser = await chromium.launch({ channel: "chrome", headless: true });
const errors = [];
async function signIn(page, email, address = base) {
  page.on("pageerror", (error) => errors.push(error.message));
  await page.goto(address);
  await page.getByLabel("Email", { exact: true }).fill(email);
  await page.getByLabel("Password", { exact: true }).fill(required("WAMN_BROWSER_PASSWORD"));
  await page.getByRole("button", { name: "Sign in", exact: true }).click();
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
  await page.getByRole("button", { name: "Send reset link", exact: true }).waitFor();
  await capture(page, "recover");
  await page.goto(`${base}/reset`);
  await page.getByRole("button", { name: "Set password", exact: true }).waitFor();
  await capture(page, "reset");
  await page.goto(base);
  await page.getByRole("button", { name: "Sign in", exact: true }).waitFor();
  await capture(page, "sign-in");
  await signIn(page, required("WAMN_BROWSER_EMAIL"));
  const audiences = page.getByRole("table", { name: "Audiences", exact: true });
  const control = audiences.getByRole("link", { name: "Control", exact: true });
  await control.waitFor();
  await audiences.getByRole("link", { name: fixture.releaseEnv, exact: true }).waitFor();
  assert.deepEqual(await audiences.getByRole("columnheader").allTextContents(), ["Org", "Project", "Environment"]);
  assert.deepEqual(await audiences.getByRole("link").allTextContents(), ["Control", fixture.releaseEnv]);
  assert.equal(await audiences.locator("details").count(), 0);
  assert.equal(await page.getByRole("navigation", { name: "Platform location" }).count(), 0);
  assert.equal(await control.getAttribute("href"), controlPath);
  assert.equal(await page.getByLabel("Email", { exact: true }).count(), 0);
  assert.equal(await page.getByLabel("Password", { exact: true }).count(), 0);
  assert.equal(await page.getByRole("button", { name: "Sign in", exact: true }).count(), 0);
  assert.equal(await audiences.getByRole("table").count(), 0);
  assert.equal(await page.locator(".platform-table-screen").getByRole("table", { name: "Audiences" }).count(), 1);
  await capture(page, "audience-choice");
  const mine = page.waitForResponse((response) => response.url().endsWith("/api/wamn_control/control/mine"));
  await control.click();
  const response = await mine;
  assert.equal(response.status(), 200);
  const held = (await response.json())[0].value;
  assert.equal(held.org_admin, true);
  if (held.projects.length === 1) {
    await page
      .getByRole("button", { name: `Change context: Project ${held.projects[0].project}`, exact: true })
      .click();
    await page.getByRole("button", { name: `Up one level: Organization ${org}`, exact: true }).click();
  }
  await page.getByRole("table", { name: "Projects", exact: true }).waitFor();
  assert.equal(new URL(page.url()).pathname, `${controlPath}/projects`);
  assert.equal(await page.getByRole("navigation", { name: "Primary navigation" }).count(), 1);
  assert.equal(await page.locator("#app > [data-ui]").count(), 1);
  assert.equal(await page.locator("#app > [data-ui]").getAttribute("data-ui-density"), "compact");

  await page
    .getByRole("table", { name: "Projects", exact: true })
    .getByRole("link", { name: fixture.releaseProject, exact: true })
    .waitFor();
  await capture(page, "control-layout");
  await capture(page, "projects");

  await page
    .getByRole("navigation", { name: "Primary navigation" })
    .getByRole("link", { name: "Users", exact: true })
    .click();
  await page.getByRole("table", { name: "Users", exact: true }).waitFor();
  await page
    .locator(".table-workspace-header-actions")
    .getByRole("button", { name: "Invite user", exact: true })
    .click();
  await page.getByRole("heading", { name: "Invite user", exact: true }).waitFor();
  await capture(page, "invite-user");
  await page.getByRole("textbox", { name: "Email", exact: true }).fill(fixture.email);
  await page.getByLabel("Display name", { exact: true }).fill("Invited User");
  await page.getByRole("button", { name: "Invite user", exact: true }).click();
  await page.getByText("User invited.", { exact: true }).waitFor();
  const invited = page
    .getByRole("table", { name: "Users", exact: true })
    .getByRole("row")
    .filter({ has: page.getByRole("cell", { name: fixture.email, exact: true }) });
  await invited.getByRole("cell", { name: "active", exact: true }).waitFor();
  await capture(page, "users");
  console.log("invitation-created");
  const { code } = JSON.parse((await messages.next()).value);
  fixture.principalId = code.slice(0, code.indexOf(":"));
  const invitedContext = await browser.newContext({ viewport });
  try {
    const enrollment = await invitedContext.newPage();
    enrollment.on("pageerror", (error) => errors.push(error.message));
    await enrollment.goto(`${base}/invite#${code}`);
    await enrollment.getByLabel("Email", { exact: true }).waitFor();
    await capture(enrollment, "invite");
    await enrollment.getByLabel("Email", { exact: true }).fill(fixture.email);
    await enrollment.getByLabel("Password", { exact: true }).fill(required("WAMN_BROWSER_PASSWORD"));
    await enrollment.getByLabel("Password again", { exact: true }).fill(required("WAMN_BROWSER_PASSWORD"));
    await enrollment.getByRole("button", { name: "Set password", exact: true }).click();
    await enrollment.getByRole("button", { name: "Sign in", exact: true }).waitFor();
    assert.equal(new URL(enrollment.url()).hash, "");
    await signIn(enrollment, fixture.email);
    await enrollment.getByRole("heading", { name: "No access", exact: true }).waitFor();
    await capture(enrollment, "no-access");
    await invited.getByRole("link", { name: "Invited User", exact: true }).click();
    await page.getByRole("heading", { name: "Invited User", exact: true }).waitFor();
    await capture(page, "user");
    await page.getByRole("button", { name: "Inactivate user", exact: true }).click();
    await page.getByText("State: inactive", { exact: true }).waitFor();
    await visit(page);
    await page.getByRole("button", { name: "Activate user", exact: true }).click();
    await page.getByText("State: active", { exact: true }).waitFor();
    await page.getByRole("button", { name: "Grant org admin", exact: true }).click();
    await page.getByRole("button", { name: "Revoke org admin", exact: true }).waitFor();
    await signIn(enrollment, fixture.email, `${base}${controlPath}`);
    await enrollment.getByRole("button", { name: /^Change context: (Organization|Project) / }).waitFor();
    await visit(page);
    await page.getByRole("button", { name: "Revoke org admin", exact: true }).click();
    await page.getByRole("button", { name: "Grant org admin", exact: true }).waitFor();
    await visit(page);
    await page.getByRole("button", { name: "Grant org admin", exact: true }).waitFor();
  } finally {
    await invitedContext.close();
  }
  await visit(page, `${base}${controlPath}/projects`);
  await page
    .locator(".table-workspace-header-actions")
    .getByRole("button", { name: "Create project", exact: true })
    .click();
  await page.getByRole("heading", { name: "Create project", exact: true }).waitFor();
  await capture(page, "create-project");
  await page.getByLabel("Project name", { exact: true }).fill(fixture.project);
  await page.getByRole("button", { name: "Create project", exact: true }).click();
  await page.getByText("Project created.", { exact: true }).waitFor();
  await page
    .getByRole("table", { name: "Projects", exact: true })
    .getByRole("link", { name: fixture.project, exact: true })
    .click();
  await page.getByRole("heading", { name: fixture.project, exact: true }).waitFor();
  assert.equal(new URL(page.url()).pathname, `${controlPath}/projects/${fixture.project}`);
  await capture(page, "project");
  await expandGroup(page, "Environments");
  await page.getByRole("link", { name: "All environments", exact: true }).click();
  const environments = page.getByRole("table", { name: "Environments", exact: true });
  await environments.waitFor();
  assert.equal(await environments.locator("tbody tr[data-row]").count(), 0);
  console.log("project-created");
  assert.equal((await messages.next()).value, "environment-ready");
  const reloadEnvironments = page.waitForResponse((response) =>
    new URL(response.url()).pathname.endsWith("/api/wamn_control/environment/list"),
  );
  await page.getByRole("button", { name: "Refresh results", exact: true }).click();
  assert.equal((await reloadEnvironments).status(), 200);
  await environments.getByRole("link", { name: fixture.env, exact: true }).waitFor();
  await capture(page, "project-environments");

  const userAddress = `${base}${controlPath}/users/${fixture.principalId}`;
  await visit(page, userAddress);
  await page.getByRole("link", { name: "Memberships", exact: true }).click();
  await page.getByRole("table", { name: "Memberships", exact: true }).waitFor();
  await page
    .locator(".table-workspace-header-actions")
    .getByRole("button", { name: "Grant membership", exact: true })
    .click();
  await page.getByRole("heading", { name: "Grant membership", exact: true }).waitFor();
  await capture(page, "user-grant-membership");
  await page.getByRole("button", { name: "Project", exact: true }).click();
  await page.getByRole("option", { name: fixture.project, exact: true }).click();
  await page.getByRole("button", { name: "Environment", exact: true }).click();
  await page.getByRole("option", { name: fixture.env, exact: true }).click();
  await page.getByRole("button", { name: "Grant membership", exact: true }).click();
  const userMemberships = page
    .getByRole("table", { name: "Memberships", exact: true })
    .getByRole("row")
    .filter({ has: page.getByRole("cell", { name: fixture.project, exact: true }) })
    .filter({ has: page.getByRole("cell", { name: fixture.env, exact: true }) });
  await userMemberships.getByRole("button", { name: "Revoke membership", exact: true }).waitFor();
  await capture(page, "user-memberships");
  await visit(page, userAddress);
  await page.getByRole("link", { name: "Project access", exact: true }).click();
  const access = page.getByRole("table", { name: "Project access", exact: true });
  await access.getByRole("link", { name: fixture.project, exact: true }).waitFor();
  await capture(page, "user-projects");
  await access.getByRole("link", { name: fixture.project, exact: true }).click();
  await page.getByRole("button", { name: "Grant project admin", exact: true }).click();
  await page.getByRole("button", { name: "Revoke project admin", exact: true }).click();
  await page.getByRole("button", { name: "Grant project admin", exact: true }).waitFor();
  await userMemberships.getByRole("button", { name: "Revoke membership", exact: true }).click();
  await userMemberships.getByRole("button", { name: "Revoke membership", exact: true }).waitFor({ state: "detached" });
  console.log(
    "PASS: Users invite, browser enrollment and sign-in, activate/deactivate, org-admin grant/revoke; User membership and project-admin grant/revoke, reload state",
  );

  const projectAddress = `${base}${controlPath}/projects/${fixture.project}`;
  await visit(page, `${projectAddress}/members`);
  await page.getByRole("table", { name: "Members", exact: true }).waitFor();
  await page
    .locator(".table-workspace-header-actions")
    .getByRole("button", { name: "Grant membership", exact: true })
    .click();
  await page.getByRole("heading", { name: "Grant membership", exact: true }).waitFor();
  await capture(page, "project-grant-membership");
  await page.getByRole("textbox", { name: "User ID", exact: true }).fill(fixture.principalId);
  await page.getByRole("button", { name: "Environment", exact: true }).click();
  await page.getByRole("option", { name: fixture.env, exact: true }).click();
  await page.getByRole("button", { name: "Grant membership", exact: true }).click();
  const person = page
    .getByRole("table", { name: "Members", exact: true })
    .locator(`tr[data-row="${fixture.principalId}"]`);
  await person.getByRole("link", { name: "Invited User", exact: true }).waitFor();
  await capture(page, "project-members");
  await person.getByRole("link", { name: "Invited User", exact: true }).click();
  const memberEnvironments = page.getByRole("table", { name: `Memberships for ${fixture.email}`, exact: true });
  await memberEnvironments.getByRole("button", { name: "Revoke membership", exact: true }).waitFor();
  await capture(page, "member-memberships");
  assert.equal(
    new URL(page.url()).pathname,
    `${controlPath}/projects/${fixture.project}/members/${fixture.principalId}`,
  );
  await page.getByRole("button", { name: "Grant project admin", exact: true }).click();
  await page.getByRole("button", { name: "Revoke project admin", exact: true }).waitFor();

  const secondContext = await browser.newContext({ viewport });
  const projectAdminStarted = performance.now();
  try {
    const second = await secondContext.newPage();
    const projectListRequests = [];
    second.on("request", (request) => {
      if (new URL(request.url()).pathname === "/api/wamn_control/project/list") projectListRequests.push(request.url());
    });
    await signIn(second, fixture.email, `${base}${controlPath}`);
    await second.getByRole("heading", { name: fixture.project, exact: true }).waitFor();
    assert.equal(new URL(second.url()).pathname, `${controlPath}/projects/${fixture.project}`);
    await second.getByRole("button", { name: `Change context: Project ${fixture.project}`, exact: true }).click();
    assert.equal(await second.locator("[data-context-option]").count(), 0);
    await second.getByRole("button", { name: `Up one level: Organization ${org}`, exact: true }).click();
    await second.getByRole("table", { name: "Projects", exact: true }).waitFor();
    assert.deepEqual(
      await second.getByRole("table", { name: "Projects", exact: true }).locator("tbody a").allTextContents(),
      [fixture.project],
    );
    assert.equal(await second.getByRole("button", { name: "Create project", exact: true }).count(), 0);
    assert.equal(await second.getByRole("link", { name: "Users", exact: true }).count(), 0);
    await second
      .getByRole("table", { name: "Projects", exact: true })
      .getByRole("link", { name: fixture.project, exact: true })
      .click();
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
      `PASS: project-admin opens its single project directly, sees only its project, calls no project.list, and cannot open receiving (${((performance.now() - projectAdminStarted) / 1000).toFixed(2)}s)`,
    );
  } finally {
    await secondContext.close();
  }

  await page.getByRole("button", { name: "Revoke project admin", exact: true }).click();
  await page.getByRole("button", { name: "Grant project admin", exact: true }).waitFor();
  await memberEnvironments.getByRole("button", { name: "Revoke membership", exact: true }).click();
  await memberEnvironments
    .getByRole("button", { name: "Revoke membership", exact: true })
    .waitFor({ state: "detached" });
  await page.getByRole("table", { name: "Members", exact: true }).waitFor();
  assert.equal(new URL(page.url()).pathname, `${controlPath}/projects/${fixture.project}/members`);
  await page
    .locator(".table-workspace-header-actions")
    .getByRole("button", { name: "Grant membership", exact: true })
    .click();
  await page.getByRole("heading", { name: "Grant membership", exact: true }).waitFor();
  await capture(page, "member-grant-membership");
  assert.equal(new URL(page.url()).pathname, `${controlPath}/projects/${fixture.project}/members/grant`);
  await page.getByRole("textbox", { name: "User ID", exact: true }).fill(fixture.principalId);
  await page.getByRole("button", { name: "Environment", exact: true }).click();
  await page.getByRole("option", { name: fixture.env, exact: true }).click();
  await page.getByRole("button", { name: "Grant membership", exact: true }).click();
  await person.getByRole("link", { name: "Invited User", exact: true }).click();
  await memberEnvironments.getByRole("button", { name: "Revoke membership", exact: true }).waitFor();
  await page
    .getByRole("navigation", { name: "Platform location" })
    .getByRole("link", { name: fixture.project.toUpperCase(), exact: true })
    .click();
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
  await expandGroup(page, "Environments");
  await page.getByRole("link", { name: "All environments", exact: true }).click();
  await page
    .getByRole("table", { name: "Environments", exact: true })
    .getByRole("link", { name: fixture.releaseEnv, exact: true })
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
  assert.equal(await page.getByRole("table").count(), 0);
  await capture(page, "environment");
  const environmentAddress = page.url();
  const environmentPathname = new URL(environmentAddress).pathname;
  for (const [label, path] of [
    ["Workloads", "workloads"],
    ["Live releases", "live-releases"],
    ["Operations", "operations"],
    ["Roles", "roles"],
  ]) {
    const profileRead =
      path === "operations"
        ? page.waitForResponse((response) =>
            new URL(response.url()).pathname.endsWith("/api/wamn_control/release/profile"),
          )
        : undefined;
    await page
      .getByRole("navigation", { name: "Primary navigation" })
      .getByRole("link", { name: label, exact: true })
      .click();
    const table = page.getByRole("table", { name: label, exact: true });
    await table.waitFor();
    assert.equal(new URL(page.url()).pathname, `${environmentPathname}/${path}`);
    assert.equal(await page.getByRole("table").count(), 1);
    assert.equal(
      await page.getByRole("navigation", { name: "Platform location" }).locator('[aria-current="page"]').textContent(),
      label.toUpperCase(),
    );
    await capture(page, `environment-${path}`);
    if (path === "workloads" || path === "operations") {
      const profile = profileRead ? (await (await profileRead).json())[0].value : undefined;
      const operation = profile?.operations.find((entry) =>
        entry.reaches.some((reach) => reach.statements.some((statement) => statement.relations.length)),
      );
      if (path === "operations") {
        assert.ok(operation, "the release has an operation with SQL relations");
        await expandReference(table, operation.operation);
      } else await table.locator("[data-subtable-toggle]").first().click();
      await page
        .getByRole("table", { name: path === "workloads" ? /^Conditions for / : "Reaches", exact: true })
        .waitFor();
      assert.equal(new URL(page.url()).pathname, `${environmentPathname}/${path}`);
      assert.ok((await page.locator("table table").count()) > 0);
      if (path === "operations") {
        const reaches = page.getByRole("table", { name: "Reaches", exact: true });
        const reach = operation.reaches.find((entry) =>
          entry.statements.some((statement) => statement.relations.length),
        );
        await expandReference(reaches, reach.package);
        const statements = page.getByRole("table", { name: "Statements", exact: true });
        await statements.waitFor();
        const statement = reach.statements.find((entry) => entry.relations.length);
        await expandReference(statements, statement.path);
        const relations = page.getByRole("table", { name: "Relations", exact: true });
        await relations.waitFor();
        await relations.locator("[data-subtable-toggle]").first().click();
        await page.getByRole("table", { name: "Read fields", exact: true }).waitFor();
        await page.getByRole("table", { name: "Write fields", exact: true }).waitFor();
      }
      await capture(page, path === "workloads" ? "workload-conditions" : "operation-references");
    }
    if (path === "roles" && (await table.locator("[data-subtable-toggle]").count())) {
      await table.locator("[data-subtable-toggle]").first().click();
      await page.getByRole("table", { name: "Role operations", exact: true }).waitFor();
    }
    await visit(page);
    await table.waitFor();
    await page
      .getByRole("navigation", { name: "Platform location" })
      .getByRole("link", { name: fixture.releaseEnv.toUpperCase(), exact: true })
      .click();
    assert.equal(page.url(), environmentAddress);
  }
  await page
    .getByRole("button", {
      name: `Change context: Environment ${fixture.releaseProject}/${fixture.releaseEnv}`,
      exact: true,
    })
    .click();
  await page.getByRole("textbox", { name: "Search contexts", exact: true }).fill(fixture.releaseEnv);
  await capture(page, "environment-switcher");
  await page.getByRole("button", { name: `Up one level: Project ${fixture.releaseProject}`, exact: true }).click();
  await page.getByRole("button", { name: `Change context: Project ${fixture.releaseProject}`, exact: true }).click();
  await page.getByRole("textbox", { name: "Search contexts", exact: true }).fill(fixture.project);
  await page.getByRole("button", { name: `Project ${fixture.project}`, exact: true }).click();
  await page.getByRole("heading", { name: fixture.project, exact: true }).waitFor();
  await expandGroup(page, "Environments");
  await page
    .getByRole("navigation", { name: "Primary navigation" })
    .getByRole("link", { name: fixture.env, exact: true })
    .click();
  await page.getByRole("link", { name: "Members", exact: true }).click();
  await page
    .getByRole("table", { name: "Members", exact: true })
    .getByRole("link", { name: "Invited User", exact: true })
    .waitFor();
  assert.equal(new URL(page.url()).pathname, `${controlPath}/projects/${fixture.project}/${fixture.env}/members`);
  await capture(page, "environment-members");
  await visit(page);
  await page
    .getByRole("button", { name: `Change context: Environment ${fixture.project}/${fixture.env}`, exact: true })
    .waitFor();
  await visit(page, environmentAddress);
  await page.getByRole("button", { name: "Inactivate environment", exact: true }).click();
  await page.getByText("State: inactive", { exact: true }).waitFor();
  assert.equal(await page.getByRole("button", { name: "Inactivate environment", exact: true }).count(), 0);
  await visit(page);
  await page.getByRole("button", { name: "Activate environment", exact: true }).click();
  await page.getByText("State: active", { exact: true }).waitFor();
  assert.equal(await page.getByRole("button", { name: "Activate environment", exact: true }).count(), 0);
  await visit(page);
  await page.getByRole("button", { name: "Inactivate environment", exact: true }).waitFor();
  await page.getByRole("region", { name: "Release profile" }).waitFor();
  assert.deepEqual(errors, []);
  console.log(
    "PASS: Environment navigation, local runtime status, release profile, single activate/inactivate action and reload state",
  );
  console.log(
    "PASS: Control hierarchy follows the address; create, membership grant/revoke, project-admin grant/revoke, scoped access, activate/inactivate and reload state",
  );
  const receiving = await browser.newPage({ viewport });
  await signIn(receiving, required("WAMN_BROWSER_EMAIL"));
  const environment = receiving
    .getByRole("table", { name: "Audiences", exact: true })
    .getByRole("row")
    .filter({ has: receiving.getByRole("cell", { name: org, exact: true }) })
    .filter({ has: receiving.getByRole("cell", { name: fixture.releaseProject, exact: true }) })
    .getByRole("link", { name: fixture.releaseEnv, exact: true });
  await environment.waitFor();
  const environmentPath = await environment.getAttribute("href");
  assert.ok(
    environmentPath.startsWith(`/urn:wamn:project-env:${org}:${fixture.releaseProject}:${fixture.releaseEnv}:`),
  );
  await environment.click();
  await receiving.getByRole("button", { name: "User menu", exact: true }).click();
  await receiving.getByRole("menuitem", { name: "Sign out", exact: true }).waitFor();
  await receiving.keyboard.press("Escape");
  assert.equal(new URL(receiving.url()).pathname, environmentPath);
  await receiving.getByRole("heading", { name: "No screens available", exact: true }).waitFor();
  assert.equal(await receiving.getByRole("navigation", { name: "Primary navigation" }).count(), 0);
  assert.equal(
    await receiving.getByRole("link", { name: "Choose an audience", exact: true }).getAttribute("href"),
    "/",
  );
  await capture(receiving, "receiving-layout");
  await receiving.close();
  console.log(
    "PASS: audience table replaces sign-in; Control first; org/project/environment rows enter their audiences",
  );
  assert.deepEqual(
    [...captured].sort(),
    [
      "sign-in",
      "invite",
      "recover",
      "reset",
      "audience-choice",
      "no-access",
      "create-project",
      "invite-user",
      "project-environments",
      "project-members",
      "project-grant-membership",
      "member-memberships",
      "member-grant-membership",
      "user-projects",
      "user-memberships",
      "user-grant-membership",
      "control-layout",
      "projects",
      "project",
      "environment",
      "environment-workloads",
      "environment-live-releases",
      "environment-operations",
      "environment-roles",
      "workload-conditions",
      "operation-references",
      "environment-switcher",
      "environment-members",
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
