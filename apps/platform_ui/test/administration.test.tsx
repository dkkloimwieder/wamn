import { fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { flush } from "solid-js";
import { describe, expect, it } from "vitest";
import { administrationSections } from "../src/administration";
import { Shell } from "../src/shell";
import { AUD, identity, input } from "./fixtures";

const READ = "sample:item/read";
const ADMIN_ONLY = "wamn-control:role/list";

function mount(path = "roles", options: { admin?: boolean; fail?: string; many?: boolean } = {}) {
  const session = identity(true, [], { admin: options.admin ?? true, permissions: [] });
  let roles = ["admin", "reader"];
  const selected = new Set<string>();
  const held = new Set<string>(["admin"]);
  const writes: { path: string; value: Record<string, string> }[] = [];
  const calls: string[] = [];
  let failure = options.fail;
  const fetch: typeof globalThis.fetch = async (url, init) => {
    const route = new URL(String(url), "http://localhost");
    const path = route.pathname;
    calls.push(path);
    const answer = (value: unknown) => new Response(JSON.stringify([{ value }]));
    if (/\/(create|delete|grant|revoke)$/.test(path)) {
      const body = JSON.parse(String(init?.body))[0];
      writes.push({ path, value: body.value });
      if (failure && path.endsWith(failure))
        return new Response(
          JSON.stringify([
            { request_id: body.request_id, error: { code: "permission_denied", detail: { operation: path } } },
          ]),
        );
      if (path.endsWith("/role/create")) roles.push(body.value.role);
      if (path.endsWith("/role/delete")) roles = roles.filter((name) => name !== body.value.role);
      if (path.endsWith("/permission/grant")) selected.add(`${body.value.role}:${body.value.operation}`);
      if (path.endsWith("/permission/revoke")) selected.delete(`${body.value.role}:${body.value.operation}`);
      if (path.endsWith("/user_role/grant")) held.add(body.value.role);
      if (path.endsWith("/user_role/revoke")) held.delete(body.value.role);
      return new Response(
        JSON.stringify([
          {
            request_id: body.request_id,
            value: { created: true, deleted: true, rows_added: 1, granted: true, revoked: true },
          },
        ]),
      );
    }
    if (path.endsWith("/role/list")) return answer({ roles });
    if (path.endsWith("/permission/list")) {
      const name = JSON.parse(route.searchParams.get("role")!) as string;
      return answer({
        role: name,
        admin: name === "admin",
        operations: [
          ...Array.from({ length: options.many ? 26 : 1 }, (_, i) => ({
            operation: i ? `${READ}-${i}` : READ,
            served: true,
            grantable: true,
            admin_only: false,
            selected: selected.has(`${name}:${READ}`),
          })),
          { operation: ADMIN_ONLY, served: true, grantable: false, admin_only: true, selected: false },
          { operation: "old:item/read", served: false, grantable: false, admin_only: false, selected: false },
        ],
      });
    }
    if (path.endsWith("/user/list"))
      return answer({
        users: Array.from({ length: options.many ? 26 : 1 }, (_, i) => ({
          id: i ? `user-${i}` : "alex",
          email: `alex${i}@example.invalid`,
          display_name: i ? `User ${i}` : "Alex",
          roles: [...held],
          admin_covered: true,
        })),
      });
    return session.fetch(url, init);
  };
  window.history.pushState({}, "", `/${AUD}/administration/${path}`);
  render(() => <Shell title="WAMN" org="acme" sections={administrationSections} fetch={fetch} />);
  return {
    writes,
    calls,
    fail: (next?: string) => {
      failure = next;
    },
  };
}

async function confirm(label: string) {
  fireEvent.click(await screen.findByRole("button", { name: label }));
  const dialog = await screen.findByRole("alertdialog");
  fireEvent.click(within(dialog).getByRole("button", { name: label }));
}

describe("application roles", () => {
  it("uses served permission rows, keeps admin read-only and admin_only disabled", async () => {
    const { writes } = mount();
    const admin = await screen.findByRole("checkbox", { name: `${READ} admin` }, { timeout: 5000 });
    expect(admin).toHaveProperty("checked", true);
    fireEvent.click(admin);
    expect(admin).toHaveProperty("checked", true);
    for (const name of ["admin", "reader"])
      expect(screen.getByRole("checkbox", { name: `${ADMIN_ONLY} ${name}` })).toHaveProperty("disabled", true);
    expect(screen.queryByRole("checkbox", { name: /old:item/ })).toBeNull();
    expect(screen.queryByRole("button", { name: "Delete role admin" })).toBeNull();
    expect(writes).toEqual([]);
  });
  it("sends exactly one command per change and shows the saved value", async () => {
    const { writes } = mount();
    await screen.findByRole("checkbox", { name: `${READ} reader` }, { timeout: 5000 });
    const cell = () => screen.getByRole("checkbox", { name: `${READ} reader` });
    fireEvent.click(cell());
    await screen.findByText(`Granted ${READ} to reader.`);
    expect(cell()).toHaveProperty("checked", true);
    expect(writes).toEqual([
      { path: "/api/wamn_control/permission/grant", value: { role: "reader", operation: READ } },
    ]);
    await waitFor(() => expect(cell()).toHaveProperty("disabled", false));
    fireEvent.click(cell());
    await waitFor(() => expect(screen.getByText(`Revoked ${READ} from reader.`)).toBeTruthy());
    expect(cell()).toHaveProperty("checked", false);
    expect(writes).toHaveLength(2);
    expect(writes[1]?.path).toBe("/api/wamn_control/permission/revoke");
  });
  it("reverts a refused cell and allows retry", async () => {
    const state = mount("roles", { fail: "/permission/grant" });
    await screen.findByRole("checkbox", { name: `${READ} reader` }, { timeout: 5000 });
    const cell = () => screen.getByRole("checkbox", { name: `${READ} reader` });
    fireEvent.click(cell());
    await screen.findByRole("alert");
    expect(cell()).toHaveProperty("checked", false);
    expect(state.writes).toHaveLength(1);
    state.fail();
    await waitFor(() => expect(cell()).toHaveProperty("disabled", false));
    fireEvent.click(cell());
    await screen.findByText(`Granted ${READ} to reader.`);
    expect(cell()).toHaveProperty("checked", true);
  });
  it("creates on its own form screen, cancels, and confirms deletion", async () => {
    const { writes } = mount();
    await screen.findByRole("checkbox", { name: `${READ} reader` }, { timeout: 5000 });
    fireEvent.click(screen.getByRole("button", { name: "Create role" }));
    input(await screen.findByRole("textbox", { name: "Role" }), "clerk");
    expect(screen.queryByRole("table")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    await screen.findByRole("table", { name: "Roles" });
    expect(writes).toHaveLength(0);
    fireEvent.click(screen.getByRole("button", { name: "Create role" }));
    input(await screen.findByRole("textbox", { name: "Role" }), "clerk");
    fireEvent.click(screen.getByRole("button", { name: "Create role" }));
    await screen.findByRole("checkbox", { name: `${READ} clerk` });
    fireEvent.click(screen.getByRole("button", { name: "Delete role clerk" }));
    fireEvent.click(within(await screen.findByRole("alertdialog")).getByRole("button", { name: "Cancel" }));
    expect(writes).toHaveLength(1);
    await confirm("Delete role clerk");
    await waitFor(() => expect(screen.queryByRole("checkbox", { name: `${READ} clerk` })).toBeNull());
    expect(writes.map((entry) => entry.path)).toEqual([
      "/api/wamn_control/role/create",
      "/api/wamn_control/role/delete",
    ]);
  });
  it("keeps failed create and delete actions visible with their errors", async () => {
    const state = mount("roles/create", { fail: "/role/create" });
    input(await screen.findByRole("textbox", { name: "Role" }), "clerk");
    fireEvent.click(screen.getByRole("button", { name: "Create role" }));
    await screen.findByText(/permission_denied/);
    expect(screen.getByRole("textbox", { name: "Role" })).toHaveProperty("value", "clerk");
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    state.fail("/role/delete");
    await confirm("Delete role reader");
    await screen.findByRole("alert");
    expect(screen.getByRole("checkbox", { name: `${READ} reader` })).toBeTruthy();
  });
  it("pages growing operation rows", async () => {
    mount("roles", { many: true });
    await screen.findByRole("checkbox", { name: `${READ} reader` }, { timeout: 5000 });
    expect(screen.queryByRole("checkbox", { name: `${READ}-25 reader` })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Next page" }));
    await screen.findByRole("checkbox", { name: `${READ}-25 reader` });
  });
});

describe("application users", () => {
  it("links the name to the roles table and grants and revokes with confirmation", async () => {
    const { writes } = mount("users");
    fireEvent.click(await screen.findByRole("link", { name: "Alex" }));
    await screen.findByRole("table", { name: "Roles for Alex" });
    expect(screen.queryByRole("button", { name: "Revoke role admin" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Grant role" }));
    fireEvent.keyDown(await screen.findByRole("button", { name: "Role" }), { key: "Enter" });
    flush();
    fireEvent.keyDown(await screen.findByRole("option", { name: "reader" }), { key: "Enter" });
    flush();
    fireEvent.click(screen.getByRole("button", { name: "Grant role" }));
    await screen.findByRole("button", { name: "Revoke role reader" });
    await confirm("Revoke role reader");
    await waitFor(() => expect(screen.queryByRole("button", { name: "Revoke role reader" })).toBeNull());
    expect(writes).toEqual([
      { path: "/api/wamn_control/user_role/grant", value: { user_id: "alex", role: "reader" } },
      { path: "/api/wamn_control/user_role/revoke", value: { user_id: "alex", role: "reader" } },
    ]);
  });
  it("keeps refused grants and revocations visible and cancels a revoke without a command", async () => {
    const state = mount("users/alex/grant", { fail: "/user_role/grant" });
    fireEvent.keyDown(await screen.findByRole("button", { name: "Role" }), { key: "Enter" });
    fireEvent.keyDown(await screen.findByRole("option", { name: "reader" }), { key: "Enter" });
    flush();
    fireEvent.click(screen.getByRole("button", { name: "Grant role" }));
    await screen.findByText(/permission_denied/);
    expect(window.location.pathname).toBe(`/${AUD}/administration/users/alex/grant`);
    state.fail();
    fireEvent.click(screen.getByRole("button", { name: "Grant role" }));
    fireEvent.click(await screen.findByRole("button", { name: "Revoke role reader" }));
    fireEvent.click(within(await screen.findByRole("alertdialog")).getByRole("button", { name: "Cancel" }));
    expect(state.writes).toHaveLength(2);
    state.fail("/user_role/revoke");
    await confirm("Revoke role reader");
    await screen.findByRole("alert");
    expect(
      within(screen.getByRole("table", { name: "Roles for Alex", hidden: true })).getByText("reader"),
    ).toBeTruthy();
  });
  it("cancels the grant form without a command", async () => {
    const { writes } = mount("users/alex/grant");
    fireEvent.click(await screen.findByRole("button", { name: "Cancel" }));
    await screen.findByRole("table", { name: "Roles for Alex" });
    expect(writes).toEqual([]);
  });
  it("pages users with one fact per column", async () => {
    mount("users", { many: true });
    await screen.findByRole("link", { name: "Alex" });
    expect(screen.getAllByRole("columnheader").map((entry) => entry.textContent)).toEqual([
      "User name",
      "Email",
      "User ID",
      "Admin covered",
    ]);
    expect(screen.queryByRole("link", { name: "User 25" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Next page" }));
    await screen.findByRole("link", { name: "User 25" });
  });
});

describe("application administration navigation", () => {
  it("shows Roles and Users in the environment context with literal uppercase breadcrumbs", async () => {
    mount();
    await screen.findByRole("button", { name: "Change context: Environment widgets/dev" });
    const pane = within(screen.getByRole("navigation", { name: "Primary navigation" }));
    expect(pane.getByRole("link", { name: "Roles" })).toBeTruthy();
    fireEvent.click(pane.getByRole("link", { name: "Users" }));
    await screen.findByRole("link", { name: "Alex" });
    expect(
      within(screen.getByRole("navigation", { name: "Platform location" }))
        .getAllByRole("listitem")
        .map((entry) => entry.querySelector("a, [aria-current]")?.textContent),
    ).toEqual(["ACME", "WIDGETS", "DEV", "USERS"]);
  });
  it.each(["roles", "roles/create", "users", "users/alex", "users/alex/grant"])(
    "hides %s and refuses direct navigation for non-admins",
    async (path) => {
      const { calls } = mount(path, { admin: false });
      await screen.findByRole("heading", { name: "No page here" });
      expect(screen.queryByRole("navigation", { name: "Primary navigation" })).toBeNull();
      expect(calls.filter((path) => path.startsWith("/api/"))).toEqual(["/api/wamn_control/permission/mine"]);
    },
  );
});
