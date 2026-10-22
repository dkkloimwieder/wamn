import { fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { describe, expect, it } from "vitest";
import { flush } from "solid-js";
import { orgAdmin, user } from "@wamn/control-org-client";
import { createTransport } from "@wamn/web-runtime";
import { controlHolds, Shell } from "../src/shell";
import { GrantMembership } from "../src/membership";
import { NotFoundBoundary } from "../src/page";
import { InviteUser, User, userSections } from "../src/users";
import { CONTROL, identity, input } from "./fixtures";

const admin = controlHolds({ orgAdmin: true, projects: [] });
const principalId = "00000000-0000-4000-8000-000000000001";
const otherId = "00000000-0000-4000-8000-000000000002";
const person = {
  principal_id: principalId,
  display_name: "Invited User",
  email: "invite@example.invalid",
  status: "active",
  org_admin: false,
};
const props = { holds: admin, params: {}, search: {}, open: () => {}, close: () => {} };
const completed = (value: unknown, requestId?: string) =>
  new Response(JSON.stringify([{ value, ...(requestId ? { request_id: requestId } : {}) }]));
const operations = [
  user.USER_LIST_ROUTE.operation,
  user.USER_INVITE_ROUTE.operation,
  user.USER_ACTIVATE_ROUTE.operation,
  user.USER_DEACTIVATE_ROUTE.operation,
  orgAdmin.ORG_ADMIN_GRANT_ROUTE.operation,
  orgAdmin.ORG_ADMIN_REVOKE_ROUTE.operation,
];

describe("Users authority", () => {
  it.each(operations)("uses control.mine for %s", (operation) => {
    expect(admin(operation)).toBe(true);
    expect(controlHolds({ orgAdmin: false, projects: [{ project: "widgets", projectAdmin: true }] })(operation)).toBe(
      false,
    );
    expect(controlHolds({ orgAdmin: false, projects: [] })(operation)).toBe(false);
  });
  it.each(["users", "invite-user", `users/${principalId}`])(
    "shows no page at %s without org authority",
    async (path) => {
      const session = identity(true, [], undefined, {
        org_admin: false,
        projects: [{ project: "widgets", project_admin: true }],
      });
      window.history.pushState({}, "", `/${CONTROL}/${path}`);
      render(() => (
        <Shell title="WAMN" org="acme" sections={[]} controlSections={userSections} fetch={session.fetch} />
      ));
      await screen.findByText("This address names no page.");
      expect(screen.queryByRole("link", { name: "Users" })).toBeNull();
      expect(session.state.calls).toEqual(["/password/renew", "/api/wamn_control/control/mine"]);
    },
  );
});

describe("Users decisions", () => {
  it.each([true, false])("invites without implicit grants and reports delivery=%s", async (delivered) => {
    const writes: unknown[] = [];
    let invited = false;
    const session = identity(true);
    const fetch: typeof globalThis.fetch = async (url, init) => {
      if (String(url).endsWith("/user/invite")) {
        const [request] = JSON.parse(String(init?.body));
        writes.push(request.value);
        invited = true;
        return completed({ principal_id: principalId, invited: delivered, enrolled: !delivered }, request.request_id);
      }
      if (String(url).includes("/user/list")) return completed({ users: invited ? [person] : [] });
      return session.fetch(url, init);
    };
    window.history.pushState({}, "", `/${CONTROL}/users`);
    render(() => <Shell title="WAMN" org="acme" sections={[]} controlSections={userSections} fetch={fetch} />);
    await screen.findByRole("table", { name: "Users" });
    const header = screen.getByRole("button", { name: "Table controls" }).closest(".table-workspace-header-actions")!;
    fireEvent.click(within(header as HTMLElement).getByRole("button", { name: "Invite user" }));
    await screen.findByRole("heading", { name: "Invite user" });
    expect(window.location.pathname).toBe(`/${CONTROL}/invite-user`);
    expect(screen.queryByRole("table", { name: "Users" })).toBeNull();
    input(screen.getByRole("textbox", { name: /^Email$/ }), person.email);
    input(screen.getByLabelText("Display name"), person.display_name);
    fireEvent.click(screen.getByRole("button", { name: "Invite user" }));
    await screen.findByText(delivered ? "User invited." : "User is already enrolled.");
    await screen.findByRole("link", { name: person.display_name });
    expect(window.location.pathname).toBe(`/${CONTROL}/users`);
    expect(screen.queryByRole("textbox", { name: "Display name" })).toBeNull();
    expect(writes).toEqual([
      { email: person.email, display_name: person.display_name, memberships: [], project_admins: [], org_admin: false },
    ]);

    const users = within(screen.getByRole("table", { name: "Users" }));
    expect(users.getAllByRole("columnheader").map((cell) => cell.textContent)).toEqual([
      "Display name",
      "Email",
      "User ID",
      "State",
      "Org admin",
    ]);
    expect(users.getByTitle(principalId).textContent).toBe("00000000");
    expect(users.getByRole("cell", { name: person.email })).toBeTruthy();
    expect(users.getByRole("cell", { name: "active" })).toBeTruthy();
    expect(users.getByRole("img", { name: "No" })).toBeTruthy();
    fireEvent.click(screen.getByRole("link", { name: person.display_name }));
    await waitFor(() => expect(window.location.pathname).toBe(`/${CONTROL}/users/${principalId}`));
  });
  it("keeps a refused invitation on the form with its values and error", async () => {
    let opened = "";
    const transport = createTransport({
      baseUrl: "/api",
      fetch: async (_url, init) => {
        const [request] = JSON.parse(String(init?.body));
        return new Response(
          JSON.stringify([
            {
              request_id: request.request_id,
              error: { code: "permission_denied", detail: { operation: user.USER_INVITE_ROUTE.operation } },
            },
          ]),
        );
      },
    });
    render(() => (
      <InviteUser
        {...props}
        transport={transport}
        open={(path) => {
          opened = path;
        }}
      />
    ));
    input(screen.getByRole("textbox", { name: "Email" }), person.email);
    input(screen.getByLabelText("Display name"), person.display_name);
    fireEvent.click(screen.getByRole("button", { name: "Invite user" }));
    expect(await screen.findByRole("alert")).toHaveProperty("textContent", "Error: permission_denied");
    expect(screen.getByRole("textbox", { name: "Email" })).toHaveProperty("value", person.email);
    expect(opened).toBe("");
  });

  it.each(["Inactivate user", "Activate user", "Grant org admin", "Revoke org admin"])(
    "refreshes after %s",
    async (action) => {
      let current = {
        ...person,
        status: action === "Activate user" ? "inactive" : "active",
        org_admin: action === "Revoke org admin",
      };
      const writes: string[] = [];
      const next: Record<string, string> = {
        "Inactivate user": "Activate user",
        "Activate user": "Inactivate user",
        "Grant org admin": "Revoke org admin",
        "Revoke org admin": "Grant org admin",
      };
      const transport = createTransport({
        baseUrl: "/api",
        fetch: async (url, init) => {
          const path = String(url);
          if (path.endsWith("/user/list")) return completed({ users: [current] });
          const [request] = JSON.parse(String(init?.body));
          expect(request.value).toEqual({ principal_id: principalId });
          writes.push(path);
          current = {
            ...current,
            status: action === "Inactivate user" ? "inactive" : "active",
            org_admin: action === "Grant org admin",
          };
          return completed(
            { principal_id: principalId, status: current.status, org_admin: current.org_admin },
            request.request_id,
          );
        },
      });
      render(() => <User {...props} params={{ principal: principalId }} transport={transport} />);
      fireEvent.click(await screen.findByRole("button", { name: action }));
      await screen.findByRole("button", { name: next[action] });
      expect(screen.queryByRole("button", { name: action })).toBeNull();
      expect(writes).toEqual([
        `/api/wamn_control/${action.includes("org admin") ? "org_admin" : "user"}/${action === "Inactivate user" ? "deactivate" : action.split(" ")[0]!.toLowerCase()}`,
      ]);
    },
  );
  it("keeps refused state and shows the server error", async () => {
    const transport = createTransport({
      baseUrl: "/api",
      fetch: async (url, init) => {
        if (String(url).endsWith("/user/list")) return completed({ users: [person] });
        const [request] = JSON.parse(String(init?.body));
        return new Response(
          JSON.stringify([
            {
              request_id: request.request_id,
              error: { code: "permission_denied", detail: { operation: user.USER_DEACTIVATE_ROUTE.operation } },
            },
          ]),
        );
      },
    });
    render(() => <User {...props} params={{ principal: principalId }} transport={transport} />);
    fireEvent.click(await screen.findByRole("button", { name: "Inactivate user" }));
    expect(await screen.findByRole("alert")).toHaveProperty("textContent", "Error: permission_denied");
    expect(screen.getByText("State: active")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Activate user" })).toBeNull();
  });
});

describe("User decisions", () => {
  it("selects a project and environment on the user membership form and returns after submission", async () => {
    let opened = "";
    const writes: unknown[] = [];
    const transport = createTransport({
      baseUrl: "/api",
      fetch: async (url, init) => {
        const path = new URL(String(url), "http://localhost").pathname;
        if (path.endsWith("/project/list")) return completed({ projects: [{ name: "widgets", state: "active" }] });
        if (path.endsWith("/environment/list")) return completed({ environments: [{ name: "dev", state: "active" }] });
        const [request] = JSON.parse(String(init?.body));
        writes.push(request.value);
        return completed({ ...request.value, member: true }, request.request_id);
      },
    });
    render(() => (
      <GrantMembership
        {...props}
        params={{ principal: principalId }}
        transport={transport}
        returnTo={`users/${principalId}/memberships`}
        open={(path) => {
          opened = path;
        }}
      />
    ));
    fireEvent.keyDown(await screen.findByRole("button", { name: "Project" }), { key: "Enter" });
    fireEvent.keyDown(await screen.findByRole("option", { name: "widgets" }), { key: "Enter" });
    flush();
    fireEvent.keyDown(screen.getByRole("button", { name: "Environment" }), { key: "Enter" });
    fireEvent.keyDown(await screen.findByRole("option", { name: "dev" }), { key: "Enter" });
    flush();
    fireEvent.click(screen.getByRole("button", { name: "Grant membership" }));
    await waitFor(() => expect(opened).toBe(`users/${principalId}/memberships`));
    expect(writes).toEqual([{ principal_id: principalId, project: "widgets", env: "dev" }]);
  });
  it("scopes all four access writes to the addressed user and reads each project", async () => {
    const calls: string[] = [];
    const writes: { path: string; value: unknown }[] = [];
    let environments: string[] = [];
    let projectAdmin = false;
    const transportFetch: typeof globalThis.fetch = async (url, init) => {
      const path = String(url);
      calls.push(path);
      if (path.includes("/user/list"))
        return completed({ users: [person, { ...person, principal_id: otherId, display_name: "Other User" }] });
      if (path.includes("/project/list"))
        return completed({
          projects: [
            { name: "widgets", state: "active" },
            { name: "gadgets", state: "active" },
          ],
        });
      if (path.includes("/environment/list")) return completed({ environments: [{ name: "dev", state: "active" }] });
      if (path.includes("/member/list"))
        return completed({
          members: [
            { ...person, environments, project_admin: projectAdmin },
            { ...person, principal_id: otherId, environments: ["other-env"], project_admin: true },
          ],
        });
      const [request] = JSON.parse(String(init?.body));
      writes.push({ path, value: request.value });
      if (path.endsWith("/member/grant")) environments = ["dev"];
      if (path.endsWith("/member/revoke")) environments = [];
      if (path.endsWith("/project_admin/grant")) projectAdmin = true;
      if (path.endsWith("/project_admin/revoke")) projectAdmin = false;
      return completed(
        { ...request.value, member: environments.length > 0, project_admin: projectAdmin },
        request.request_id,
      );
    };
    const session = identity(true);
    window.history.pushState({}, "", `/${CONTROL}/users/${principalId}`);
    render(() => (
      <Shell
        title="WAMN"
        org="acme"
        sections={[]}
        controlSections={userSections}
        fetch={(url, init) =>
          String(url).includes("/api/wamn_control/") && !String(url).endsWith("/control/mine")
            ? transportFetch(url, init)
            : session.fetch(url, init)
        }
      />
    ));
    await screen.findByRole("heading", { name: person.display_name });
    expect(screen.getByText(person.email)).toBeTruthy();
    expect(screen.getByText("State: active")).toBeTruthy();
    expect(screen.getByTitle(principalId)).toBeTruthy();
    expect(screen.queryByRole("table")).toBeNull();
    fireEvent.click(screen.getByRole("link", { name: "Project access" }));
    const table = await screen.findByRole("table", { name: "Project access" });
    expect(screen.getAllByRole("table")).toHaveLength(1);
    await within(table).findByRole("link", { name: "widgets" });
    const row = within(table.querySelector<HTMLElement>('tr[data-row="widgets"]')!);
    expect(row.getByRole("cell", { name: person.display_name })).toBeTruthy();
    expect(row.getByRole("cell", { name: person.email })).toBeTruthy();
    expect(row.getByTitle(principalId)).toBeTruthy();
    expect(row.getAllByRole("img", { name: "No" })).toHaveLength(2);
    expect(calls.filter((path) => path.includes("/member/list"))).toHaveLength(2);
    expect(screen.queryByText("other-env")).toBeNull();
    fireEvent.click(within(table).getByRole("link", { name: "widgets" }));
    await screen.findByRole("table", { name: "Memberships" });
    fireEvent.click(screen.getByRole("button", { name: "Grant membership" }));
    await screen.findByRole("heading", { name: "Grant membership" });
    expect(screen.queryByRole("table")).toBeNull();
    fireEvent.keyDown(await screen.findByRole("button", { name: "Environment" }), { key: "Enter" });
    fireEvent.keyDown(await screen.findByRole("option", { name: "dev" }), { key: "Enter" });
    flush();
    fireEvent.click(screen.getByRole("button", { name: "Grant membership" }));
    await screen.findByRole("button", { name: "Revoke membership" });
    expect(window.location.pathname).toBe(`/${CONTROL}/users/${principalId}/projects/widgets`);
    const memberships = within(screen.getByRole("table", { name: "Memberships" }));
    expect(memberships.getAllByRole("columnheader").map((cell) => cell.textContent)).toEqual([
      "Project",
      "Environment",
      "State",
      "Action",
    ]);
    expect(memberships.getByRole("cell", { name: "active" })).toBeTruthy();
    expect(screen.queryByRole("form")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Grant project admin" }));
    const revokeAdmin = await screen.findByRole("button", { name: "Revoke project admin" });
    expect(revokeAdmin.textContent).toBe("Revoke project admin");
    fireEvent.click(revokeAdmin);
    await screen.findByRole("button", { name: "Grant project admin" });
    const revoke = memberships.getByRole("button", { name: "Revoke membership" });
    expect(revoke.textContent).toBe("");
    expect(revoke.title).toBe("Revoke membership");
    expect(revoke.classList.contains("ui-icon-button")).toBe(true);
    expect(revoke.querySelector('svg[aria-hidden="true"] path')?.getAttribute("d")).toBe("M3 3l10 10M13 3 3 13");
    fireEvent.click(revoke);
    await waitFor(() => expect(screen.queryByRole("button", { name: "Revoke membership" })).toBeNull());
    expect(writes).toEqual([
      { path: "/api/wamn_control/member/grant", value: { principal_id: principalId, project: "widgets", env: "dev" } },
      { path: "/api/wamn_control/project_admin/grant", value: { principal_id: principalId, project: "widgets" } },
      { path: "/api/wamn_control/project_admin/revoke", value: { principal_id: principalId, project: "widgets" } },
      { path: "/api/wamn_control/member/revoke", value: { principal_id: principalId, project: "widgets", env: "dev" } },
    ]);
  });
  it("does not offer another user's grants when the addressed user is absent", async () => {
    const transport = createTransport({
      baseUrl: "/api",
      fetch: async (url) =>
        completed(
          String(url).includes("/user/list") ? { users: [{ ...person, principal_id: otherId }] } : { projects: [] },
        ),
    });
    render(() => (
      <NotFoundBoundary home={`/${CONTROL}`} path="missing">
        <User {...props} params={{ principal: principalId }} transport={transport} />
      </NotFoundBoundary>
    ));
    await screen.findByRole("heading", { name: "No page here" });
    expect(document.querySelector(".ui-dashboard")).toBeNull();
    expect(screen.queryByRole("button", { name: "Grant membership" })).toBeNull();
  });
});
