import { fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { describe, expect, it } from "vitest";
import { flush } from "solid-js";
import { orgAdmin, user } from "@wamn/control-org-client";
import { createTransport } from "@wamn/web-runtime";
import { controlHolds, Shell } from "../src/shell";
import { User, Users, userSections } from "../src/users";
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
  it.each(["users", `users/${principalId}`])("shows no page at %s without org authority", async (path) => {
    const session = identity(true, [], undefined, {
      org_admin: false,
      projects: [{ project: "widgets", project_admin: true }],
    });
    window.history.pushState({}, "", `/${CONTROL}/${path}`);
    render(() => <Shell title="WAMN" org="acme" sections={[]} controlSections={userSections} fetch={session.fetch} />);
    await screen.findByText("This address names no page.");
    expect(screen.queryByRole("link", { name: "Users" })).toBeNull();
    expect(session.state.calls).toEqual(["/password/renew", "/api/wamn_control/control/mine"]);
  });
});

describe("Users decisions", () => {
  it.each([true, false])("invites without implicit grants and reports delivery=%s", async (delivered) => {
    const writes: unknown[] = [];
    let invited = false;
    let opened = "";
    const transport = createTransport({
      baseUrl: "/api",
      fetch: async (url, init) => {
        if (String(url).endsWith("/user/invite")) {
          const [request] = JSON.parse(String(init?.body));
          writes.push(request.value);
          invited = true;
          return completed({ principal_id: principalId, invited: delivered, enrolled: !delivered }, request.request_id);
        }
        return completed({ users: invited ? [person] : [] });
      },
    });
    render(() => (
      <Users
        {...props}
        transport={transport}
        open={(path) => {
          opened = path;
        }}
      />
    ));
    input(screen.getByRole("textbox", { name: /^Email$/ }), person.email);
    input(screen.getByLabelText("Display name"), person.display_name);
    fireEvent.click(screen.getByRole("button", { name: "Invite user" }));
    await screen.findByText(delivered ? "User invited." : "User is already enrolled.");
    fireEvent.click(await screen.findByRole("button", { name: person.display_name }));
    expect(writes).toEqual([
      { email: person.email, display_name: person.display_name, memberships: [], project_admins: [], org_admin: false },
    ]);
    expect(opened).toBe(`users/${principalId}`);
  });
  it.each(["Deactivate user", "Activate user", "Grant org admin", "Revoke org admin"])(
    "refreshes after %s",
    async (action) => {
      let current = {
        ...person,
        status: action === "Activate user" ? "inactive" : "active",
        org_admin: action === "Revoke org admin",
      };
      const writes: string[] = [];
      const next: Record<string, string> = {
        "Deactivate user": "Activate user",
        "Activate user": "Deactivate user",
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
            status: action === "Deactivate user" ? "inactive" : "active",
            org_admin: action === "Grant org admin",
          };
          return completed(
            { principal_id: principalId, status: current.status, org_admin: current.org_admin },
            request.request_id,
          );
        },
      });
      render(() => <Users {...props} transport={transport} />);
      fireEvent.click(await screen.findByRole("button", { name: action }));
      await screen.findByRole("button", { name: next[action] });
      expect(screen.queryByRole("button", { name: action })).toBeNull();
      expect(writes).toEqual([
        `/api/wamn_control/${action.includes("org admin") ? "org_admin" : "user"}/${action.split(" ")[0]!.toLowerCase()}`,
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
    render(() => <Users {...props} transport={transport} />);
    fireEvent.click(await screen.findByRole("button", { name: "Deactivate user" }));
    expect(await screen.findByRole("alert")).toHaveProperty("textContent", "Error: permission_denied");
    expect(screen.getByText("User state: active")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Activate user" })).toBeNull();
  });
});

describe("User decisions", () => {
  it("scopes all four access writes to the addressed user and reads each project", async () => {
    const calls: string[] = [];
    const writes: { path: string; value: unknown }[] = [];
    let environments: string[] = [];
    let projectAdmin = false;
    const transport = createTransport({
      baseUrl: "/api",
      fetch: async (url, init) => {
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
      },
    });
    render(() => <User {...props} params={{ principal: principalId }} transport={transport} />);
    await screen.findByRole("heading", { name: person.display_name });
    expect(screen.queryByText("other-env")).toBeNull();
    expect(calls.filter((path) => path.includes("/member/list"))).toHaveLength(2);
    fireEvent.keyDown(screen.getByRole("button", { name: "Project" }), { key: "Enter" });
    fireEvent.keyDown(await screen.findByRole("option", { name: "widgets" }), { key: "Enter" });
    flush();
    fireEvent.keyDown(screen.getByRole("button", { name: "Environment" }), { key: "Enter" });
    fireEvent.keyDown(await screen.findByRole("option", { name: "dev" }), { key: "Enter" });
    flush();
    fireEvent.click(screen.getByRole("button", { name: "Grant membership" }));
    const access = () =>
      within(
        screen.getByRole("table", { name: "Project access" }).querySelector<HTMLElement>('tr[data-row="widgets"]')!,
      );
    await waitFor(() => expect(access().getByRole("button", { name: "Revoke membership in dev" })).toBeTruthy());
    fireEvent.click(access().getByRole("button", { name: "Grant project admin" }));
    await waitFor(() => expect(access().getByText("Project admin")).toBeTruthy());
    fireEvent.click(access().getByRole("button", { name: "Revoke project admin" }));
    await waitFor(() => expect(access().getByText("User")).toBeTruthy());
    fireEvent.click(access().getByRole("button", { name: "Revoke membership in dev" }));
    await waitFor(() => expect(access().queryByRole("button", { name: "Revoke membership in dev" })).toBeNull());
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
    render(() => <User {...props} params={{ principal: principalId }} transport={transport} />);
    expect(await screen.findByRole("alert")).toHaveProperty("textContent", "Error: The org has no such user.");
    expect(screen.queryByRole("button", { name: "Grant membership" })).toBeNull();
  });
});
