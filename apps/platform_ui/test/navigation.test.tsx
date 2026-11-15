import { fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { flush } from "solid-js";
import { describe, expect, it } from "vitest";
import { createTransport } from "@wamn/web-runtime";
import { createAddress, matchPath } from "../src/address";
import { ControlNavigation } from "../src/control-navigation";
import { Shell } from "../src/shell";
// The shell loads its layout lazily. Loading it here keeps module transform
// time out of the first render that the tests wait for.
import "../src/layout";
import { controlSections } from "../src/projects";
import { userSections } from "../src/users";
import { CONTROL, identity, input } from "./fixtures";

function mount(path: string, orgAdmin = true, projects = ["widgets", "gadgets"]) {
  const session = identity(true, [], undefined, {
    org_admin: orgAdmin,
    projects: projects.map((project) => ({ project, project_admin: true })),
  });
  const calls: string[] = [];
  const writes: unknown[] = [];
  let members = [
    {
      principal_id: "alex",
      display_name: "Alex",
      email: "alex@example.invalid",
      org_admin: false,
      project_admin: false,
      environments: ["dev", "prod"],
    },
    {
      principal_id: "sam",
      display_name: "Sam",
      email: "sam@example.invalid",
      org_admin: false,
      project_admin: true,
      environments: ["prod"],
    },
  ];
  // As the server does, an environment keeps its members and only its name.
  const environmentMembers = (encoded: string | null) => {
    if (encoded === null) return members;
    const environment: string = JSON.parse(encoded);
    return members
      .filter((entry) => entry.environments.includes(environment))
      .map((entry) => ({ ...entry, environments: [environment] }));
  };
  const fetch: typeof globalThis.fetch = async (url, init) => {
    const route = new URL(String(url), "http://localhost").pathname;
    calls.push(route);
    if (/\/(grant|revoke)$/.test(route)) {
      const body = JSON.parse(String(init?.body))[0];
      writes.push(body.value);
      if (route.endsWith("/member/revoke"))
        members = members.map((entry) =>
          entry.principal_id === body.value.principal_id
            ? { ...entry, environments: entry.environments.filter((env) => env !== body.value.env) }
            : entry,
        );
      return new Response(JSON.stringify([{ request_id: body.request_id, value: {} }]));
    }
    const value = route.endsWith("/project/list")
      ? { projects: projects.map((name) => ({ name, state: "active" })) }
      : route.endsWith("/environment/list")
        ? {
            environments: [
              { name: "dev", state: "active" },
              { name: "prod", state: "active" },
            ],
          }
        : route.endsWith("/member/list")
          ? { members: environmentMembers(new URL(String(url), "http://localhost").searchParams.get("environment")) }
          : route.endsWith("/user/list")
            ? { users: members.map((entry) => ({ ...entry, status: "active" })) }
            : route.endsWith("/environment/status")
              ? {
                  digest: null,
                  workloads: [],
                  live: [],
                  residency: [],
                  rollout: { replicas: 0, updated_replicas: 0, ready_replicas: 0, available_replicas: 0 },
                }
              : undefined;
    return value ? new Response(JSON.stringify([{ value }])) : session.fetch(url, init);
  };
  window.history.pushState({}, "", `/${CONTROL}${path ? `/${path}` : ""}`);
  render(() => (
    <Shell
      title="WAMN"
      org="acme"
      sections={[]}
      controlSections={[...controlSections, ...userSections]}
      fetch={fetch}
    />
  ));
  return { calls, writes };
}

async function switcher(name: string) {
  return screen.findByRole("button", { name: `Change context: ${name}` });
}
function disclose(label: string) {
  fireEvent.click(within(screen.getByRole("navigation", { name: "Primary navigation" })).getByText(label));
  flush();
}

describe("control hierarchy navigation", () => {
  it.each([
    ["projects", "Organization acme", "Projects"],
    ["create-project", "Organization acme", "Projects"],
    ["users", "Organization acme", "Users"],
    ["invite-user", "Organization acme", "Users"],
    ["users/alex", "Organization acme", "Users"],
    ["users/alex/projects", "Organization acme", "Project access"],
    ["users/alex/projects/widgets", "Organization acme", "Project access"],
    ["users/alex/projects/widgets/grant", "Organization acme", "Project access"],
    ["users/alex/memberships", "Organization acme", "Memberships"],
    ["users/alex/memberships/grant", "Organization acme", "Memberships"],
    ["projects/widgets", "Project widgets", "widgets"],
    ["projects/widgets/environments", "Project widgets", "Environments"],
    ["projects/widgets/members", "Project widgets", "Members"],
    ["projects/widgets/members/grant", "Project widgets", "Members"],
    ["projects/widgets/members/alex", "Project widgets", "Members"],
    ["projects/widgets/members/alex/grant", "Project widgets", "Members"],
    ["projects/widgets/dev", "Environment widgets/dev", "dev"],
    ["projects/widgets/dev/workloads", "Environment widgets/dev", "Workloads"],
    ["projects/widgets/dev/live-releases", "Environment widgets/dev", "Live releases"],
    ["projects/widgets/dev/operations", "Environment widgets/dev", "Operations"],
    ["projects/widgets/dev/roles", "Environment widgets/dev", "Roles"],
    ["projects/widgets/dev/members", "Environment widgets/dev", "Members"],
    ["projects/widgets/dev/members/grant", "Environment widgets/dev", "Members"],
    ["projects/widgets/dev/members/alex", "Environment widgets/dev", "Members"],
    ["projects/widgets/dev/members/alex/grant", "Environment widgets/dev", "Members"],
  ])("opens the route context and its navigation controls at %s", async (path, context, selected) => {
    mount(path);
    await switcher(context);
    await waitFor(() =>
      expect(
        within(screen.getByRole("navigation", { name: "Primary navigation" })).getAllByText(selected).length,
      ).toBeGreaterThan(0),
    );
    if (context === "Organization acme") {
      const pane = within(screen.getByRole("navigation", { name: "Primary navigation" }));
      expect(pane.getByRole("link", { name: "widgets" }).closest("details")?.open).toBe(true);
      expect(pane.getByRole("link", { name: "gadgets" })).toBeTruthy();
    }
  });

  it.each([
    ["projects/widgets", "Project widgets", "Projects", "widgets"],
    ["projects/widgets/dev", "Environment widgets/dev", "Environments", "dev"],
    ["users/alex/projects/widgets", "Organization acme", "Users", "Project access"],
  ])("collapses and reopens the current group at %s", async (path, context, group, selected) => {
    window.history.pushState({}, "", `/${CONTROL}/${path}`);
    const params = [...controlSections, ...userSections]
      .flatMap((section) => [...section.screens, ...(section.routes ?? [])])
      .map((route) => matchPath(route.path, path))
      .find((params) => params !== undefined)!;
    const transport = createTransport({
      baseUrl: "http://localhost",
      fetch: async () =>
        new Response(JSON.stringify([{ value: { environments: [{ name: "dev", state: "active" }] } }])),
    });
    render(() => (
      <nav aria-label="Primary navigation">
        <ControlNavigation
          org="acme"
          aud={CONTROL}
          mine={{ orgAdmin: true, projects: [{ project: "widgets", projectAdmin: true }] }}
          holds={() => true}
          params={params}
          address={createAddress()}
          transport={transport}
          navigation={{ compact: () => false, close: () => {}, mobile: () => false }}
        />
      </nav>
    ));
    await switcher(context);
    await waitFor(() =>
      expect(
        within(screen.getByRole("navigation", { name: "Primary navigation" })).getAllByText(selected).length,
      ).toBeGreaterThan(0),
    );
    disclose(group);
    expect(
      within(screen.getByRole("navigation", { name: "Primary navigation" }))
        .getByText(group)
        .closest("details")?.open,
    ).toBe(false);
    disclose(group);
    await waitFor(() =>
      expect(
        within(screen.getByRole("navigation", { name: "Primary navigation" })).getAllByText(selected).length,
      ).toBeGreaterThan(0),
    );
  });

  it.each([
    ["projects/new-project", "Project new-project", "new-project"],
    ["projects/widgets/new-env", "Environment widgets/new-env", "new-env"],
  ])("opens the current context before its list includes it at %s", async (path, context, selected) => {
    mount(path);
    await switcher(context);
    expect(
      within(screen.getByRole("navigation", { name: "Primary navigation" })).getAllByText(selected).length,
    ).toBeGreaterThan(0);
  });

  it("expands held projects and switches project and environment contexts with searchable siblings and parents", async () => {
    mount("projects");
    await switcher("Organization acme");
    const pane = within(screen.getByRole("navigation", { name: "Primary navigation" }));
    expect(pane.getByRole("link", { name: "Users" })).toBeTruthy();
    expect(
      within(screen.getByRole("navigation", { name: "Primary navigation" })).getAllByText("Projects").length,
    ).toBeGreaterThan(0);
    fireEvent.click(pane.getByRole("link", { name: "widgets" }));
    await switcher("Project widgets");
    expect(pane.queryByRole("link", { name: "Users" })).toBeNull();
    expect(pane.getByRole("link", { name: "Members" }).getAttribute("href")).toBe(
      `/${CONTROL}/projects/widgets/members`,
    );
    fireEvent.click(await switcher("Project widgets"));
    input(await screen.findByRole("textbox", { name: "Search contexts" }), "gad");
    fireEvent.click(screen.getByRole("button", { name: "Project gadgets" }));
    await switcher("Project gadgets");
    expect(window.location.pathname).toBe(`/${CONTROL}/projects/gadgets`);
    fireEvent.click(await pane.findByRole("link", { name: "dev" }));
    await switcher("Environment gadgets/dev");
    await pane.findByRole("link", { name: "prod" });

    fireEvent.click(await switcher("Environment gadgets/dev"));
    input(await screen.findByRole("textbox", { name: "Search contexts" }), "prod");
    fireEvent.click(screen.getByRole("button", { name: "Environment gadgets/prod" }));
    await switcher("Environment gadgets/prod");
    fireEvent.click(await switcher("Environment gadgets/prod"));
    fireEvent.click(await screen.findByRole("button", { name: "Up one level: Project gadgets" }));
    await switcher("Project gadgets");
    fireEvent.click(await switcher("Project gadgets"));
    fireEvent.click(await screen.findByRole("button", { name: "Up one level: Organization acme" }));
    await screen.findByRole("heading", { name: "Control" });
    expect(window.location.pathname).toBe(`/${CONTROL}`);
  });

  it.each([
    ["projects/widgets/environments", "Project widgets"],
    ["projects/widgets/members/grant", "Project widgets"],
    ["projects/widgets/dev/roles", "Environment widgets/dev"],
    ["projects/widgets/dev/members/alex", "Environment widgets/dev"],
    ["users/alex/projects/widgets", "Organization acme"],
  ])("derives the initial pane from %s and follows browser history", async (path, context) => {
    mount(path);
    await switcher(context);
    window.history.pushState({}, "", `/${CONTROL}/projects/gadgets/prod/operations`);
    window.dispatchEvent(new PopStateEvent("popstate"));
    await switcher("Environment gadgets/prod");
    const pane = within(screen.getByRole("navigation", { name: "Primary navigation" }));
    expect(pane.getByRole("link", { name: "Operations" }).getAttribute("aria-current")).toBe("page");
    expect(
      within(screen.getByRole("navigation", { name: "Primary navigation" })).getAllByText("Operations").length,
    ).toBeGreaterThan(0);
  });

  it("opens the Control splash for multiple held projects", async () => {
    mount("");
    await screen.findByRole("heading", { name: "Control" });
    expect(window.location.pathname).toBe(`/${CONTROL}`);
    await screen.findByRole("region", { name: "widgets" });
    await screen.findByRole("region", { name: "gadgets" });
    expect(
      within(screen.getByRole("navigation", { name: "Primary navigation" })).getAllByText("Control home").length,
    ).toBeGreaterThan(0);
  });

  it.each([true, false])("opens one held project directly (org admin: %s), and allows return to org", async (admin) => {
    const { calls } = mount("", admin, ["widgets"]);
    await screen.findByRole("heading", { name: "widgets" });
    expect(window.location.pathname).toBe(`/${CONTROL}/projects/widgets`);
    expect(
      within(screen.getByRole("navigation", { name: "Primary navigation" })).getAllByText("widgets").length,
    ).toBeGreaterThan(0);
    fireEvent.click(await switcher("Project widgets"));
    fireEvent.click(await screen.findByRole("button", { name: "Up one level: Organization acme" }));
    await screen.findByRole("heading", { name: "Control" });
    expect(window.location.pathname).toBe(`/${CONTROL}`);
    const pane = within(screen.getByRole("navigation", { name: "Primary navigation" }));
    expect(!!pane.queryByRole("link", { name: "Users" })).toBe(admin);
    expect(
      within(screen.getByRole("navigation", { name: "Primary navigation" })).getAllByText("Control home").length,
    ).toBeGreaterThan(0);
    if (!admin) expect(calls.some((path) => path.endsWith("/project/list"))).toBe(false);
  });

  it("keeps user access links in the org pane and removes the old link row", async () => {
    mount("users/alex");
    await screen.findByRole("heading", { name: "Alex" });
    const pane = within(screen.getByRole("navigation", { name: "Primary navigation" }));
    fireEvent.click(pane.getByRole("link", { name: "Project access" }));
    await screen.findByRole("table", { name: "Project access" });
    await switcher("Organization acme");
    fireEvent.click(pane.getByRole("link", { name: "Memberships" }));
    await screen.findByRole("table", { name: "Memberships" });
  });

  it("views Members, membership actions and grants to the current environment", async () => {
    const { writes } = mount("projects/widgets/dev/members");
    const table = within(await screen.findByRole("table", { name: "Members" }));
    fireEvent.click(await table.findByRole("link", { name: "Alex" }));
    expect(table.queryByRole("link", { name: "Sam" })).toBeNull();
    const memberships = within(await screen.findByRole("table", { name: "Memberships for alex@example.invalid" }));
    expect(memberships.queryByRole("link", { name: "prod" })).toBeNull();
    fireEvent.click(memberships.getByRole("button", { name: "Revoke membership" }));
    await waitFor(() => expect(window.location.pathname).toBe(`/${CONTROL}/projects/widgets/dev/members`));
    await screen.findByRole("table", { name: "Members" });
    fireEvent.click(await screen.findByRole("button", { name: "Grant membership" }));
    await screen.findByRole("textbox", { name: "User ID" });
    flush();
    input(screen.getByRole("textbox", { name: "User ID" }), "alex");
    expect(screen.getByRole("textbox", { name: "User ID" })).toHaveProperty("value", "alex");
    expect(screen.queryByRole("button", { name: "Environment" })).toBeNull();
    fireEvent.click(await screen.findByRole("button", { name: "Grant membership" }));
    await waitFor(() => {
      expect(writes).toHaveLength(2);
      for (const write of writes) {
        expect(write).toHaveProperty("project", "widgets");
        expect(write).toHaveProperty("env", "dev");
        expect(write).toHaveProperty("principal_id", "alex");
      }
    });
    await waitFor(() => expect(window.location.pathname).toBe(`/${CONTROL}/projects/widgets/dev/members`));
    await switcher("Environment widgets/dev");
  });
});
