import { fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { describe, expect, it } from "vitest";
import { createTransport } from "@wamn/web-runtime";
import { Environment } from "../src/environment";
import { controlHolds, Shell } from "../src/shell";
import { controlSections, Project } from "../src/projects";
import { CONTROL, identity } from "./fixtures";

function fixture(initial: "active" | "inactive" = "active", digest: string | null = "sha256:release", refuse = false) {
  let state = initial;
  const calls: string[] = [];
  const writes: unknown[] = [];
  const fetch: typeof globalThis.fetch = async (url, init) => {
    const path = String(url);
    calls.push(path);
    if (path.endsWith("/environment/activate") || path.endsWith("/environment/inactivate")) {
      const body = JSON.parse(String(init?.body));
      writes.push(body[0].value);
      if (refuse) return new Response("write failed", { status: 503 });
      state = path.endsWith("/environment/activate") ? "active" : "inactive";
      return new Response(
        JSON.stringify([{ request_id: body[0].request_id, value: { project: "widgets", env: "dev", status: state } }]),
      );
    }
    const value = path.includes("/member/list")
      ? { members: [] }
      : path.includes("/environment/list")
        ? { environments: [{ name: "dev", state }] }
        : path.includes("/environment/status")
          ? {
              digest,
              rollout: { replicas: 2, updated_replicas: 1, ready_replicas: 1, available_replicas: 1 },
              workloads: [{ name: "http", conditions: [{ type: "Ready", status: "True" }] }],
              live: [{ digest: "sha256:release", pods: 1 }],
            }
          : {
              digest,
              operations: [
                {
                  package: "acme",
                  component: "receiving",
                  operation: "acme:receiving/record",
                  reaches: [
                    {
                      package: "acme",
                      operations: ["acme:receiving/record"],
                      imprecise: false,
                      statements: [
                        {
                          path: "record.sql",
                          refused: null,
                          relations: [
                            { table: "receipt", reads: ["id"], writes: ["quantity"], locks: true, deletes: false },
                          ],
                        },
                      ],
                    },
                  ],
                },
              ],
              roles: [{ role: "clerk", operations: ["acme:receiving/record"] }],
            };
    return new Response(JSON.stringify([{ value }]));
  };
  return { fetch, calls, writes, transport: createTransport({ baseUrl: "/api", fetch }) };
}

function mount(data: ReturnType<typeof fixture>, orgAdmin = true) {
  render(() => (
    <Environment
      transport={data.transport}
      holds={controlHolds({ orgAdmin, projects: [{ project: "widgets", projectAdmin: true }] })}
      params={{ project: "widgets", env: "dev" }}
      search={{}}
      open={() => {}}
      close={() => {}}
    />
  ));
}

describe("Environment", () => {
  it("opens an environment from its project", async () => {
    const data = fixture();
    let opened = "";
    render(() => (
      <Project
        transport={data.transport}
        holds={controlHolds({ orgAdmin: false, projects: [{ project: "widgets", projectAdmin: true }] })}
        params={{ project: "widgets" }}
        search={{}}
        open={(path) => {
          opened = path;
        }}
        close={() => {}}
      />
    ));
    fireEvent.click(await screen.findByRole("button", { name: "dev" }));
    expect(opened).toBe("projects/widgets/dev");
  });

  it("shows status and the selected release profile", async () => {
    const data = fixture();
    mount(data);
    await screen.findByRole("region", { name: "Release profile" });
    expect(screen.getByRole("region", { name: "Environment status" }).textContent).toContain("sha256:release");
    const conditions = within(screen.getByRole("table", { name: "Conditions for http" }));
    expect(conditions.getAllByRole("columnheader").map((cell) => cell.textContent)).toEqual(["Condition", "Status"]);
    expect(conditions.getAllByRole("cell").map((cell) => cell.textContent)).toEqual(["Ready", "True"]);
    expect(screen.getByRole("table", { name: "Live releases" }).textContent).toContain("1 pods");
    expect(within(screen.getByRole("table", { name: "Live releases" })).getByTitle("sha256:release").textContent).toBe(
      "sha256:r",
    );
    expect(screen.getByRole("table", { name: "Operations" }).textContent).toContain(
      "receipt: reads id; writes quantity; locks yes; deletes no",
    );
    expect(screen.getByRole("table", { name: "Roles" }).textContent).toContain("clerk: acme:receiving/record");
    expect(data.calls.filter((path) => path.includes("/release/profile"))).toHaveLength(1);
  });

  it.each(["active", "inactive"] as const)(
    "has one action for %s and rereads state after its write",
    async (initial) => {
      const data = fixture(initial);
      mount(data);
      const action = initial === "active" ? "Inactivate environment" : "Activate environment";
      const opposite = initial === "active" ? "Activate environment" : "Inactivate environment";
      fireEvent.click(await screen.findByRole("button", { name: action }));
      await screen.findByRole("button", { name: opposite });
      expect(screen.queryByRole("button", { name: action })).toBeNull();
      expect(data.writes).toEqual([{ project: "widgets", env: "dev" }]);
      expect(data.calls.filter((path) => path.includes("/environment/list"))).toHaveLength(2);
      await waitFor(() => expect(data.calls.filter((path) => path.includes("/release/profile"))).toHaveLength(2));
    },
  );

  it("keeps the current action and reports an unsuccessful write", async () => {
    const data = fixture("active", "sha256:release", true);
    mount(data);
    fireEvent.click(await screen.findByRole("button", { name: "Inactivate environment" }));
    await screen.findByRole("alert");
    expect(screen.queryByRole("button", { name: "Activate environment" })).toBeNull();
    expect(screen.getByText("Environment state: active")).toBeTruthy();
  });

  it("does not request a profile when no release is installed", async () => {
    const data = fixture("inactive", null);
    mount(data);
    await screen.findByText("Release: No release");
    expect(screen.queryByRole("region", { name: "Release profile" })).toBeNull();
    expect(data.calls.some((path) => path.includes("/release/profile"))).toBe(false);
  });

  it("lets a project admin read its environment but offers no status writes", async () => {
    const data = fixture();
    mount(data, false);
    await screen.findByRole("region", { name: "Release profile" });
    expect(screen.queryByRole("button", { name: /environment$/ })).toBeNull();
  });

  it("shows no page and sends no environment read outside the project admin's scope", async () => {
    const session = identity(true, [], undefined, {
      org_admin: false,
      projects: [{ project: "widgets", project_admin: true }],
    });
    window.history.pushState({}, "", `/${CONTROL}/projects/other/dev`);
    render(() => (
      <Shell title="WAMN" org="acme" sections={[]} controlSections={controlSections} fetch={session.fetch} />
    ));
    await screen.findByText("This address names no page.");
    expect(session.state.calls).toEqual(["/password/renew", "/api/wamn_control/control/mine"]);
  });
});
