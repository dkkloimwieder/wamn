import { fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { describe, expect, it } from "vitest";
import { Shell } from "../src/shell";
import { controlSections } from "../src/projects";
import { userSections } from "../src/users";
import { CONTROL, identity } from "./fixtures";

const principal = "12345678-1234-4000-8000-000000000001";
const member = {
  principal_id: principal,
  display_name: "Alex",
  email: "alex@example.invalid",
  org_admin: false,
  project_admin: false,
  environments: ["dev"],
};
function mount(path: string, missing = false) {
  const session = identity(true);
  const calls: string[] = [];
  const fetch: typeof globalThis.fetch = async (url, init) => {
    const route = new URL(String(url), "http://localhost").pathname;
    calls.push(route);
    const value = route.endsWith("/project/list")
      ? { projects: missing ? [] : [{ name: "widgets", state: "active" }] }
      : route.endsWith("/user/list")
        ? { users: missing ? [] : [{ ...member, status: "active" }] }
        : route.endsWith("/member/list")
          ? { members: missing ? [] : [member] }
          : route.endsWith("/environment/list")
            ? { environments: missing ? [] : [{ name: "dev", state: "active" }] }
            : route.endsWith("/environment/status")
              ? {
                  digest: null,
                  workloads: [],
                  live: [],
                  rollout: { replicas: 0, updated_replicas: 0, ready_replicas: 0, available_replicas: 0 },
                }
              : undefined;
    return value ? new Response(JSON.stringify([{ value }])) : session.fetch(url, init);
  };
  window.history.pushState({}, "", `/${CONTROL}/${path}`);
  render(() => (
    <Shell
      title="WAMN"
      org="acme"
      sections={[]}
      controlSections={[...controlSections, ...userSections]}
      fetch={fetch}
    />
  ));
  return calls;
}

describe("uniform record screens", () => {
  it.each([
    ["projects", "Projects", "/project/list"],
    ["users", "Users", "/user/list"],
    ["projects/widgets/environments", "Environments", "/environment/list"],
    ["projects/widgets/members", "Members", "/member/list"],
    [`projects/widgets/members/${principal}`, "Memberships for alex@example.invalid", "/member/list"],
    [`users/${principal}/projects`, "Project access", "/member/list"],
    [`users/${principal}/memberships`, "Memberships", "/member/list"],
    [`users/${principal}/projects/widgets`, "Memberships", "/member/list"],
  ])("reloads the data for %s through the shared refresh control", async (path, title, route) => {
    const calls = mount(path);
    await screen.findByRole("table", { name: title }, { timeout: 5000 });
    await waitFor(() =>
      expect(screen.getByRole("table", { name: title }).querySelectorAll("tbody tr[data-row]").length).toBeGreaterThan(
        0,
      ),
    );
    expect(screen.getAllByRole("table")).toHaveLength(1);
    expect(document.querySelector(".ui-dashboard")?.getAttribute("data-scroll")).toBe("content");
    if (title === "Projects")
      await within(screen.getByRole("table", { name: title })).findByRole("cell", { name: "active" });
    const before = calls.filter((call) => call.endsWith(route)).length;
    fireEvent.click(screen.getByRole("button", { name: "Refresh results" }));
    await waitFor(() => expect(calls.filter((call) => call.endsWith(route))).toHaveLength(before + 1));
    const location = within(screen.getByRole("navigation", { name: "Platform location" }));
    for (const entry of location.getAllByRole("listitem"))
      expect(entry.textContent).toBe(entry.textContent?.toUpperCase());
  });

  it.each(["projects/missing", `users/${principal}`, "projects/widgets/missing"])(
    "replaces missing record %s with the standalone page",
    async (path) => {
      mount(path, true);
      await screen.findByRole("heading", { name: "No page here" }, { timeout: 5000 });
      expect(document.querySelector(".ui-dashboard, header, nav, form")).toBeNull();
      expect(screen.getByRole("main").querySelector(".ui-entry-panel")?.getAttribute("data-size")).toBe("viewport");
    },
  );

  it.each([
    ["create-project", "Create project", "projects", "Projects"],
    ["invite-user", "Invite user", "users", "Users"],
    ["projects/widgets/members/grant", "Grant membership", "projects/widgets/members", "Members"],
    [
      `projects/widgets/members/${principal}/grant`,
      "Grant membership",
      `projects/widgets/members/${principal}`,
      "Memberships for alex@example.invalid",
    ],
    [`users/${principal}/memberships/grant`, "Grant membership", `users/${principal}/memberships`, "Memberships"],
    [
      `users/${principal}/projects/widgets/grant`,
      "Grant membership",
      `users/${principal}/projects/widgets`,
      "Memberships",
    ],
  ])(
    "keeps Cancel in the shared form layout and returns %s to its owning table",
    async (path, title, destination, table) => {
      const calls = mount(path);
      await screen.findByRole("button", { name: title }, { timeout: 5000 });
      const cancel = screen.getByRole("button", { name: "Cancel" });
      expect(cancel.closest("form.ui-form")?.contains(screen.getByRole("button", { name: title }))).toBe(true);
      const form = cancel.closest("form.ui-form")!;
      const rows = form.querySelectorAll(":scope > .ui-form-actions");
      expect(rows).toHaveLength(1);
      expect(within(rows[0] as HTMLElement).getAllByRole("button")).toEqual([
        screen.getByRole("button", { name: title }),
        cancel,
      ]);
      expect(screen.queryByRole("table")).toBeNull();
      fireEvent.click(cancel);
      await screen.findByRole("table", { name: table });
      expect(window.location.pathname).toBe(`/${CONTROL}/${destination}`);
      expect(calls.some((call) => /\/(create|invite|grant)$/.test(call))).toBe(false);
    },
  );

  it("shows a missing member without leaving an empty record", async () => {
    mount("projects/widgets/members/missing");
    await screen.findByRole("heading", { name: "No page here" });
    expect(screen.queryByRole("table")).toBeNull();
  });

  it("keeps record links native for modified clicks", async () => {
    mount("projects");
    const link = await within(await screen.findByRole("table", { name: "Projects" })).findByRole("link", {
      name: "widgets",
    });
    expect(link.getAttribute("href")).toBe(`/${CONTROL}/projects/widgets`);
    const event = new MouseEvent("click", { button: 0, ctrlKey: true, bubbles: true, cancelable: true });
    link.dispatchEvent(event);
    expect(event.defaultPrevented).toBe(false);
    expect(window.location.pathname).toBe(`/${CONTROL}/projects`);
    fireEvent.click(link);
    await screen.findByRole("heading", { name: "widgets" });
    expect(screen.queryByRole("table")).toBeNull();
    const actions = screen.getByRole("group", { name: "Record actions" });
    expect(actions.parentElement?.querySelector("h1")?.textContent).toBe("widgets");
  });
});
