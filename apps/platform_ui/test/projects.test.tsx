import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { describe, expect, it } from "vitest";
import { control, environment, member, project, projectAdmin, release, user } from "@wamn/control-org-client";
import { createTransport } from "@wamn/web-runtime";
import { controlHolds, Shell } from "../src/shell";
import { controlSections, Project, Projects } from "../src/projects";
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
    window.history.pushState({}, "", `/${CONTROL}`);
    render(() => (
      <Shell title="WAMN" org="acme" sections={[]} controlSections={controlSections} fetch={session.fetch} />
    ));
    await screen.findByRole("button", { name: "widgets" });
    expect(window.location.pathname).toBe(`/${CONTROL}/projects`);
    expect(screen.queryByRole("button", { name: "gadgets" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Create project" })).toBeNull();
    expect(session.state.calls).toEqual(["/password/renew", "/api/wamn_control/control/mine"]);
  });
  it("lands Control on Projects and reads control.mine once", async () => {
    const session = identity(true);
    const fetch: typeof globalThis.fetch = async (url, init) =>
      String(url).startsWith("/api/wamn_control/project/list")
        ? new Response(JSON.stringify([{ value: { projects: [{ name: "widgets", state: "active" }] } }]))
        : session.fetch(url, init);
    window.history.pushState({}, "", `/${CONTROL}`);
    render(() => <Shell title="WAMN" org="acme" sections={[]} controlSections={controlSections} fetch={fetch} />);
    await screen.findByRole("heading", { name: "Projects" });
    await screen.findByRole("button", { name: "widgets" });
    expect(window.location.pathname).toBe(`/${CONTROL}/projects`);
    expect(session.state.calls.filter((path) => path.endsWith("/control/mine"))).toHaveLength(1);
  });

  it("creates a project, refreshes the list, and opens its encoded address", async () => {
    const projects: string[] = [];
    const writes: unknown[] = [];
    let opened = "";
    const transport = createTransport({
      baseUrl: "/api",
      fetch: async (url, init) => {
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
      },
    });
    render(() => (
      <Projects
        transport={transport}
        holds={admin}
        params={{}}
        search={{}}
        open={(path) => {
          opened = path;
        }}
        close={() => {}}
      />
    ));
    input(screen.getByLabelText("Project name"), "widgets");
    fireEvent.click(screen.getByRole("button", { name: "Create project" }));
    fireEvent.click(await screen.findByRole("button", { name: "widgets" }));
    expect(writes).toEqual([[{ request_id: expect.any(String), value: { project: "widgets" } }]]);
    expect(opened).toBe("projects/widgets");
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
      <Project
        transport={transport}
        holds={admin}
        params={{ project: "widgets" }}
        search={{}}
        open={() => {}}
        close={() => {}}
      />
    ));
    await waitFor(() =>
      expect(screen.getByRole("table", { name: "Environments" }).querySelector("tbody")?.textContent).toBe("dev"),
    );
    fireEvent.click(screen.getByRole("button", { name: "Grant membership" }));
    expect(await screen.findByRole("alert")).toHaveProperty(
      "textContent",
      "Enter a user ID and select an environment.",
    );
    expect(screen.queryByText("Membership granted.")).toBeNull();
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
    expect(screen.getByText("Project state: inactive")).toBeTruthy();
  });
});
