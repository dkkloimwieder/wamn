import { fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { flush } from "solid-js";
import { describe, expect, it } from "vitest";
import { control, environment, member, project, projectAdmin, release, user } from "@wamn/control-org-client";
import { createTransport } from "@wamn/web-runtime";
import { controlHolds, Shell } from "../src/shell";
import { GrantMembership } from "../src/membership";
import { controlSections, Project } from "../src/projects";
import { CONTROL, identity, input } from "./fixtures";

const admin = controlHolds({ orgAdmin: true, projects: [] });
const projectAdminOnly = controlHolds({
  orgAdmin: false,
  projects: [
    { project: "widgets", projectAdmin: true },
    { project: "gadgets", projectAdmin: false },
  ],
});
const projectOperations = [
  environment.ENVIRONMENT_LIST_ROUTE.operation,
  environment.ENVIRONMENT_STATUS_ROUTE.operation,
  release.RELEASE_PROFILE_ROUTE.operation,
  member.MEMBER_LIST_ROUTE.operation,
  member.MEMBER_GRANT_ROUTE.operation,
  member.MEMBER_REVOKE_ROUTE.operation,
  projectAdmin.PROJECT_ADMIN_GRANT_ROUTE.operation,
  projectAdmin.PROJECT_ADMIN_REVOKE_ROUTE.operation,
];
const orgOperations = [
  user.USER_LIST_ROUTE.operation,
  project.PROJECT_LIST_ROUTE.operation,
  project.PROJECT_CREATE_ROUTE.operation,
  project.PROJECT_ACTIVATE_ROUTE.operation,
  project.PROJECT_INACTIVATE_ROUTE.operation,
  environment.ENVIRONMENT_ACTIVATE_ROUTE.operation,
  environment.ENVIRONMENT_INACTIVATE_ROUTE.operation,
];

describe("control holds", () => {
  it("admits the Projects screen through control.mine only for administrators", () => {
    expect(admin(control.CONTROL_MINE_ROUTE.operation)).toBe(true);
    expect(projectAdminOnly(control.CONTROL_MINE_ROUTE.operation)).toBe(true);
    expect(controlHolds({ orgAdmin: false, projects: [] })(control.CONTROL_MINE_ROUTE.operation)).toBe(false);
  });
  it.each([...projectOperations, ...orgOperations])(
    "admits an org admin to %s, including newly created projects",
    (operation) => {
      expect(admin(operation)).toBe(true);
      expect(admin(operation, "new-project")).toBe(true);
    },
  );
  it.each(projectOperations)("admits %s only in the caller's administered projects", (operation) => {
    expect(projectAdminOnly(operation)).toBe(true);
    expect(projectAdminOnly(operation, "widgets")).toBe(true);
    expect(projectAdminOnly(operation, "gadgets")).toBe(false);
    expect(projectAdminOnly(operation, "unknown")).toBe(false);
  });
  it.each(orgOperations)("hides the org operation %s from project admins", (operation) => {
    expect(projectAdminOnly(operation)).toBe(false);
  });
  it("grants no control screen to a member without administration", () => {
    const holds = controlHolds({ orgAdmin: false, projects: [{ project: "widgets", projectAdmin: false }] });
    for (const operation of [...projectOperations, ...orgOperations]) expect(holds(operation, "widgets")).toBe(false);
    expect(admin("unknown:operation@1.0.0")).toBe(false);
  });
});

describe("Projects and Project", () => {
  it("lists only control.mine projects for a project admin without calling project.list", async () => {
    const session = identity(true, [], undefined, {
      org_admin: false,
      projects: [
        { project: "widgets", project_admin: true },
        { project: "gadgets", project_admin: false },
      ],
    });
    window.history.pushState({}, "", `/${CONTROL}/projects`);
    render(() => (
      <Shell title="WAMN" org="acme" sections={[]} controlSections={controlSections} fetch={session.fetch} />
    ));
    await within(await screen.findByRole("table", { name: "Projects" })).findByRole(
      "link",
      { name: "widgets" },
      { timeout: 5000 },
    );
    expect(window.location.pathname).toBe(`/${CONTROL}/projects`);
    expect(screen.queryByRole("link", { name: "gadgets" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Create project" })).toBeNull();
    const table = within(screen.getByRole("table", { name: "Projects" }));
    expect(table.getAllByRole("columnheader").map((cell) => cell.textContent)).toEqual([
      "Project",
      "Project admin",
      "Org admin",
    ]);
    expect(table.getByRole("img", { name: "Yes" })).toBeTruthy();
    expect(table.getByRole("img", { name: "No" })).toBeTruthy();
    expect(session.state.calls).toEqual(["/password/renew", "/api/wamn_control/control/mine"]);
    fireEvent.click(screen.getByRole("button", { name: "Refresh results" }));
    await waitFor(() => expect(session.state.calls.filter((call) => call.endsWith("/control/mine"))).toHaveLength(2));
  });
  it("lands Control on Projects and reads control.mine once", async () => {
    const session = identity(true);
    const fetch: typeof globalThis.fetch = async (url, init) =>
      String(url).startsWith("/api/wamn_control/project/list")
        ? new Response(JSON.stringify([{ value: { projects: [{ name: "widgets", state: "active" }] } }]))
        : session.fetch(url, init);
    window.history.pushState({}, "", `/${CONTROL}`);
    render(() => <Shell title="WAMN" org="acme" sections={[]} controlSections={controlSections} fetch={fetch} />);
    await screen.findByRole("table", { name: "Projects" });
    await within(screen.getByRole("table", { name: "Projects" })).findByRole("link", { name: "widgets" });
    expect(
      await within(screen.getByRole("table", { name: "Projects" })).findByRole("cell", { name: "active" }),
    ).toBeTruthy();
    expect(screen.getByRole("columnheader", { name: "State" })).toBeTruthy();
    expect(window.location.pathname).toBe(`/${CONTROL}/projects`);
    expect(session.state.calls.filter((path) => path.endsWith("/control/mine"))).toHaveLength(1);
  });

  it("creates a project named create and keeps its detail address reachable", async () => {
    const projects: string[] = [];
    const writes: unknown[] = [];
    const session = identity(true);
    const fetch: typeof globalThis.fetch = async (url, init) => {
      if (!String(url).includes("/api/wamn_control/project/")) return session.fetch(url, init);
      if (String(url).endsWith("/project/create")) {
        const body = JSON.parse(String(init?.body));
        writes.push(body);
        projects.push(body[0].value.project);
        return new Response(
          JSON.stringify([{ request_id: body[0].request_id, value: { project: body[0].value.project } }]),
        );
      }
      return new Response(
        JSON.stringify([{ value: { projects: projects.map((name) => ({ name, state: "inactive" })) } }]),
      );
    };
    window.history.pushState({}, "", `/${CONTROL}/projects`);
    render(() => <Shell title="WAMN" org="acme" sections={[]} controlSections={controlSections} fetch={fetch} />);
    await screen.findByRole("table", { name: "Projects" });
    const header = screen.getByRole("button", { name: "Table controls" }).closest(".table-workspace-header-actions")!;
    expect(document.querySelector(".ui-dashboard")?.getAttribute("data-scroll")).toBe("content");
    fireEvent.click(within(header as HTMLElement).getByRole("button", { name: "Create project" }));
    await screen.findByRole("heading", { name: "Create project" });
    expect(window.location.pathname).toBe(`/${CONTROL}/create-project`);
    expect(document.querySelector(".ui-dashboard")?.getAttribute("data-scroll")).toBe("main");
    expect(screen.queryByRole("table", { name: "Projects" })).toBeNull();
    input(screen.getByLabelText("Project name"), "create");
    fireEvent.click(screen.getByRole("button", { name: "Create project" }));
    await screen.findByRole("link", { name: "create" });
    expect(window.location.pathname).toBe(`/${CONTROL}/projects`);
    expect(document.querySelector(".ui-dashboard")?.getAttribute("data-scroll")).toBe("content");
    expect(screen.queryByLabelText("Project name")).toBeNull();
    expect(await screen.findByText("Project created.")).toHaveProperty("role", "status");
    expect(writes).toEqual([[{ request_id: expect.any(String), value: { project: "create" } }]]);

    expect(
      within(screen.getByRole("table", { name: "Projects" })).getByRole("cell", { name: "inactive" }),
    ).toBeTruthy();
    fireEvent.click(screen.getByRole("link", { name: "create" }));
    await waitFor(() => expect(window.location.pathname).toBe(`/${CONTROL}/projects/create`));
  });

  it("opens the create form directly and cancels to Projects without writing", async () => {
    const session = identity(true);
    const fetch: typeof globalThis.fetch = async (url, init) =>
      String(url).includes("/project/list")
        ? new Response(JSON.stringify([{ value: { projects: [] } }]))
        : session.fetch(url, init);
    window.history.pushState({}, "", `/${CONTROL}/create-project`);
    render(() => <Shell title="WAMN" org="acme" sections={[]} controlSections={controlSections} fetch={fetch} />);
    await screen.findByRole("heading", { name: "Create project" });
    const location = within(screen.getByRole("navigation", { name: "Platform location" }));
    expect(location.getByRole("link", { name: "PROJECTS" })).toBeTruthy();
    expect(document.querySelector('.ui-sidebar-group-panel[data-ui-active="true"]')).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    await screen.findByRole("table", { name: "Projects" });
    expect(window.location.pathname).toBe(`/${CONTROL}/projects`);
    expect(session.state.calls).toEqual(["/password/renew", "/api/wamn_control/control/mine"]);
  });

  it("rejects incomplete membership input without claiming success", async () => {
    const transport = createTransport({
      baseUrl: "/api",
      fetch: async (url) => {
        const value = String(url).includes("/environment/list")
          ? { environments: [{ name: "dev", state: "active" }] }
          : String(url).includes("/project/list")
            ? { projects: [{ name: "widgets", state: "active" }] }
            : { members: [] };
        return new Response(JSON.stringify([{ value }]));
      },
    });
    render(() => (
      <GrantMembership
        returnTo="projects/widgets/members"
        transport={transport}
        holds={admin}
        params={{ project: "widgets" }}
        search={{}}
        open={() => {}}
        close={() => {}}
      />
    ));
    fireEvent.click(await screen.findByRole("button", { name: "Grant membership" }));
    expect(await screen.findByRole("alert")).toHaveProperty(
      "textContent",
      "Enter a user ID and select a project and an environment.",
    );
    expect(screen.queryByText("Membership granted.")).toBeNull();
  });

  it("shows every environment and member field and links to the member screen", async () => {
    let opened = "";
    const principalId = "12345678-1234-4000-8000-000000000001";
    const calls: string[] = [];
    const transport = createTransport({
      baseUrl: "/api",
      fetch: async (url) => {
        const path = new URL(String(url), "http://localhost").pathname;
        calls.push(path);
        const value = path.endsWith("/environment/list")
          ? { environments: [{ name: "dev", state: "inactive" }] }
          : path.endsWith("/project/list")
            ? { projects: [{ name: "widgets", state: "inactive" }] }
            : {
                members: [
                  {
                    principal_id: principalId,
                    display_name: "Alex",
                    email: "alex@example.invalid",
                    org_admin: true,
                    project_admin: true,
                    environments: ["dev"],
                  },
                ],
              };
        return new Response(JSON.stringify([{ value }]));
      },
    });
    render(() => (
      <Project
        table="members"
        transport={transport}
        holds={admin}
        params={{ project: "widgets" }}
        search={{}}
        open={(path) => {
          opened = path;
        }}
        close={() => {}}
      />
    ));
    await screen.findByText("Alex");
    const members = screen.getByRole("table", { name: "Members" });
    expect(Array.from(members.querySelectorAll(":scope > thead th")).map((cell) => cell.textContent)).toEqual([
      "Display name",
      "Email",
      "User ID",
      "Org admin",
      "Project admin",
      "Memberships",
    ]);
    expect(within(members).getByText("alex@example.invalid")).toBeTruthy();
    expect(within(members).getAllByRole("img", { name: "Yes" })).toBeTruthy();
    expect(within(members).getByTitle(principalId).textContent).toBe("12345678");
    expect(screen.queryByRole("table", { name: "Memberships for alex@example.invalid" })).toBeNull();
    fireEvent.click(within(members).getByRole("link", { name: "Alex" }));
    expect(opened).toBe(`projects/widgets/members/${principalId}`);
    expect(members.querySelector("table")).toBeNull();
    expect(calls.sort()).toEqual([
      "/api/wamn_control/environment/list",
      "/api/wamn_control/member/list",
      "/api/wamn_control/project/list",
    ]);
  });
});

describe("Project member", () => {
  it.each(["Revoke membership", "Revoke project admin"])(
    "returns to Members when %s removes the member from the project",
    async (action) => {
      const principalId = "12345678-1234-4000-8000-000000000001";
      let removed = false;
      const session = identity(true, [], undefined, {
        org_admin: false,
        projects: [{ project: "widgets", project_admin: true }],
      });
      const fetch: typeof globalThis.fetch = async (url, init) => {
        const path = new URL(String(url), "http://localhost").pathname;
        if (path.endsWith("/environment/list"))
          return new Response(JSON.stringify([{ value: { environments: [{ name: "dev", state: "active" }] } }]));
        if (path.endsWith("/member/list"))
          return new Response(
            JSON.stringify([
              {
                value: {
                  members: removed
                    ? []
                    : [
                        {
                          principal_id: principalId,
                          display_name: "Alex",
                          email: "alex@example.invalid",
                          org_admin: false,
                          project_admin: action === "Revoke project admin",
                          environments: action === "Revoke membership" ? ["dev"] : [],
                        },
                      ],
                },
              },
            ]),
          );
        if (path.endsWith("/revoke")) {
          const [request] = JSON.parse(String(init?.body));
          removed = true;
          return new Response(
            JSON.stringify([
              {
                request_id: request.request_id,
                value: { ...request.value, member: false, project_admin: false },
              },
            ]),
          );
        }
        return session.fetch(url, init);
      };
      window.history.pushState({}, "", `/${CONTROL}/projects/widgets/members/${principalId}`);
      render(() => <Shell title="WAMN" org="acme" sections={[]} controlSections={controlSections} fetch={fetch} />);
      fireEvent.click(await screen.findByRole("button", { name: action }));
      await screen.findByRole("table", { name: "Members" });
      expect(window.location.pathname).toBe(`/${CONTROL}/projects/widgets/members`);
      expect(screen.queryByRole("link", { name: "Alex" })).toBeNull();
      expect(screen.queryByRole("heading", { name: "No page here" })).toBeNull();
      fireEvent.click(screen.getByRole("button", { name: "Grant membership" }));
      await screen.findByRole("heading", { name: "Grant membership" });
      expect(window.location.pathname).toBe(`/${CONTROL}/projects/widgets/members/grant`);
      expect(await screen.findByRole("textbox", { name: "User ID" })).toBeTruthy();
    },
  );

  it("lets a project admin open a member and grant or revoke one membership per command", async () => {
    const principalId = "12345678-1234-4000-8000-000000000001";
    let environments = ["dev"];
    const writes: unknown[] = [];
    const session = identity(true, [], undefined, {
      org_admin: false,
      projects: [{ project: "widgets", project_admin: true }],
    });
    const fetch: typeof globalThis.fetch = async (url, init) => {
      const path = new URL(String(url), "http://localhost").pathname;
      if (path.endsWith("/environment/list"))
        return new Response(JSON.stringify([{ value: { environments: [{ name: "dev", state: "active" }] } }]));
      if (path.endsWith("/member/list"))
        return new Response(
          JSON.stringify([
            {
              value: {
                members: [
                  {
                    principal_id: principalId,
                    display_name: "Alex",
                    email: "alex@example.invalid",
                    org_admin: false,
                    project_admin: false,
                    environments,
                  },
                ],
              },
            },
          ]),
        );
      if (path.endsWith("/member/grant") || path.endsWith("/member/revoke")) {
        const [request] = JSON.parse(String(init?.body));
        writes.push({ path, value: request.value });
        environments = path.endsWith("/grant") ? ["dev"] : [];
        return new Response(
          JSON.stringify([
            { request_id: request.request_id, value: { ...request.value, member: environments.length > 0 } },
          ]),
        );
      }
      return session.fetch(url, init);
    };
    window.history.pushState({}, "", `/${CONTROL}/projects/widgets/members`);
    render(() => <Shell title="WAMN" org="acme" sections={[]} controlSections={controlSections} fetch={fetch} />);
    fireEvent.click(await screen.findByRole("link", { name: "Alex" }));
    const table = await screen.findByRole("table", { name: "Memberships for alex@example.invalid" });
    expect(window.location.pathname).toBe(`/${CONTROL}/projects/widgets/members/${principalId}`);
    expect(table.closest("td")).toBeNull();
    expect(screen.queryByRole("table", { name: "Members" })).toBeNull();
    const breadcrumbs = within(screen.getByRole("navigation", { name: "Platform location" }));
    expect(breadcrumbs.getByRole("link", { name: "MEMBERS" })).toBeTruthy();
    expect(breadcrumbs.getByRole("link", { name: "WIDGETS" })).toBeTruthy();
    fireEvent.click(within(table).getByRole("button", { name: "Revoke membership" }));
    await waitFor(() => expect(screen.queryByRole("button", { name: "Revoke membership" })).toBeNull());
    fireEvent.click(screen.getByRole("button", { name: "Grant membership" }));
    await screen.findByRole("heading", { name: "Grant membership" });
    expect(screen.queryByRole("table")).toBeNull();
    fireEvent.keyDown(await screen.findByRole("button", { name: "Environment" }), { key: "Enter" });
    fireEvent.keyDown(await screen.findByRole("option", { name: "dev" }), { key: "Enter" });
    flush();
    fireEvent.click(screen.getByRole("button", { name: "Grant membership" }));
    await screen.findByRole("button", { name: "Revoke membership" });
    expect(writes).toEqual([
      { path: "/api/wamn_control/member/revoke", value: { project: "widgets", env: "dev", principal_id: principalId } },
      { path: "/api/wamn_control/member/grant", value: { project: "widgets", env: "dev", principal_id: principalId } },
    ]);
    expect(session.state.calls).toEqual(["/password/renew", "/api/wamn_control/control/mine"]);
  });
});

describe("project state action", () => {
  it.each(["active", "inactive"] as const)(
    "shows only the action for %s and refreshes after writing",
    async (initial) => {
      let state = initial;
      const writes: string[] = [];
      const transport = createTransport({
        baseUrl: "/api",
        fetch: async (url, init) => {
          const path = String(url);
          if (path.endsWith("/project/activate") || path.endsWith("/project/inactivate")) {
            writes.push(path);
            const body = JSON.parse(String(init?.body));
            expect(body[0].value).toEqual({ project: "widgets" });
            state = state === "active" ? "inactive" : "active";
            return new Response(
              JSON.stringify([{ request_id: body[0].request_id, value: { project: "widgets", status: state } }]),
            );
          }
          const value = path.includes("/project/list")
            ? { projects: [{ name: "widgets", state }] }
            : path.includes("/environment/list")
              ? { environments: [] }
              : { members: [] };
          return new Response(JSON.stringify([{ value }]));
        },
      });
      render(() => (
        <Project
          transport={transport}
          holds={admin}
          params={{ project: "widgets" }}
          search={{}}
          open={() => {}}
          close={() => {}}
        />
      ));
      const action = initial === "active" ? "Inactivate project" : "Activate project";
      const next = initial === "active" ? "Activate project" : "Inactivate project";
      await screen.findByRole("button", { name: action });
      expect(screen.queryByRole("button", { name: next })).toBeNull();
      fireEvent.click(screen.getByRole("button", { name: action }));
      await screen.findByRole("button", { name: next });
      expect(screen.queryByRole("button", { name: action })).toBeNull();
      expect(writes).toEqual([
        initial === "active" ? "/api/wamn_control/project/inactivate" : "/api/wamn_control/project/activate",
      ]);
    },
  );

  it("keeps an empty project inactive after a successful activation", async () => {
    const transport = createTransport({
      baseUrl: "/api",
      fetch: async (url, init) => {
        const path = String(url);
        const body = init?.body ? JSON.parse(String(init.body)) : undefined;
        const value = path.endsWith("/project/activate")
          ? { project: "widgets", status: "active" }
          : path.includes("/project/list")
            ? { projects: [{ name: "widgets", state: "inactive" }] }
            : path.includes("/environment/list")
              ? { environments: [] }
              : { members: [] };
        return new Response(
          JSON.stringify([{ ...(body?.[0]?.request_id ? { request_id: body[0].request_id } : {}), value }]),
        );
      },
    });
    render(() => (
      <Project
        transport={transport}
        holds={admin}
        params={{ project: "widgets" }}
        search={{}}
        open={() => {}}
        close={() => {}}
      />
    ));
    fireEvent.click(await screen.findByRole("button", { name: "Activate project" }));
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Activate project" }).hasAttribute("disabled")).toBe(false),
    );
    expect(screen.queryByRole("button", { name: "Inactivate project" })).toBeNull();
    expect(screen.getByText("State: inactive")).toBeTruthy();
  });
});
