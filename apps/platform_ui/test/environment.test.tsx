import { fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { describe, expect, it } from "vitest";
import { createTransport } from "@wamn/web-runtime";
import { Environment } from "../src/environment";
import { controlHolds, Shell } from "../src/shell";
import { controlSections, Project } from "../src/projects";
import { CONTROL, identity, input } from "./fixtures";

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
              users_with_role: 3,
              roles: 4,
              memberships: 5,
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
      params={{ aud: CONTROL, project: "widgets", env: "dev" }}
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
        table="environments"
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
    fireEvent.click(await screen.findByRole("link", { name: "dev" }));
    expect(opened).toBe("projects/widgets/dev");
  });

  it("shows status and the selected release profile", async () => {
    const data = fixture();
    mount(data);
    await screen.findByRole("region", { name: "Release profile" });
    expect(screen.getByRole("region", { name: "Environment status" }).textContent).toContain("release");
    expect(screen.queryByRole("table")).toBeNull();
    expect(screen.queryByRole("navigation", { name: "Environment tables" })).toBeNull();
    expect(data.calls.filter((path) => path.includes("/release/profile"))).toHaveLength(1);
    expect(data.calls.filter((path) => path.includes("/environment/status"))).toHaveLength(1);
    expect(screen.getByRole("group", { name: "Roles" }).textContent).toContain("4");
  });

  it("expands read-only tables under workloads, roles and operations without changing the address", async () => {
    const data = fixture();
    const session = identity(true);
    const base = `/${CONTROL}/projects/widgets/dev`;
    window.history.pushState({}, "", base);
    render(() => (
      <Shell
        title="WAMN"
        org="acme"
        sections={[]}
        controlSections={controlSections}
        fetch={(url, init) =>
          String(url).includes("/api/wamn_control/") && !String(url).endsWith("/control/mine")
            ? data.fetch(url, init)
            : session.fetch(url, init)
        }
      />
    ));
    fireEvent.click(await screen.findByRole("link", { name: "Workloads" }));
    fireEvent.click(await screen.findByRole("button", { name: "Expand details for row http" }));
    const conditions = within(await screen.findByRole("table", { name: "Conditions for http" }));
    expect(conditions.getAllByRole("cell").map((cell) => cell.textContent)).toEqual(["Ready", "True"]);
    expect(window.location.pathname).toBe(`${base}/workloads`);
    expect(screen.getByRole("table", { name: "Conditions for http" }).closest("td")).not.toBeNull();
    expect(screen.getAllByRole("button", { name: "Refresh results" })).toHaveLength(1);
    fireEvent.click(screen.getByRole("button", { name: "Collapse details for row http" }));
    await waitFor(() => expect(screen.queryByRole("table", { name: "Conditions for http" })).toBeNull());
    fireEvent.click(screen.getByRole("link", { name: "Roles" }));
    fireEvent.click(await screen.findByRole("button", { name: "Expand details for row clerk" }));
    expect(
      within(await screen.findByRole("table", { name: "Role operations" })).getByRole("cell", {
        name: "acme:receiving/record",
      }),
    ).toBeTruthy();
    expect(window.location.pathname).toBe(`${base}/roles`);
    fireEvent.click(screen.getByRole("link", { name: "Operations" }));
    fireEvent.click(await screen.findByRole("button", { name: "Expand details for row acme:receiving/record" }));
    fireEvent.click(await screen.findByRole("button", { name: "Expand details for row acme" }));
    expect(await screen.findByRole("table", { name: "Reach operations" })).toBeTruthy();
    fireEvent.click(await screen.findByRole("button", { name: "Expand details for row record.sql" }));
    const relations = within(await screen.findByRole("table", { name: "Relations" }));
    expect(relations.getByRole("cell", { name: "receipt" })).toBeTruthy();
    expect(relations.getByRole("img", { name: "Yes" })).toBeTruthy();
    expect(relations.getByRole("img", { name: "No" })).toBeTruthy();
    fireEvent.click(relations.getByRole("button", { name: "Expand details for row receipt" }));
    expect(
      within(await screen.findByRole("table", { name: "Read fields" })).getByRole("cell", { name: "id" }),
    ).toBeTruthy();
    expect(
      within(screen.getByRole("table", { name: "Write fields" })).getByRole("cell", { name: "quantity" }),
    ).toBeTruthy();
    expect(window.location.pathname).toBe(`${base}/operations`);
    expect(screen.getAllByRole("button", { name: "Refresh results" })).toHaveLength(1);
    expect(screen.getAllByRole("navigation", { name: "Table pages" })).toHaveLength(1);
    expect(
      within(screen.getByRole("navigation", { name: "Platform location" }))
        .getByText("OPERATIONS")
        .getAttribute("aria-current"),
    ).toBe("page");
  });

  it.each([
    ["workloads", "Workloads"],
    ["live-releases", "Live releases"],
    ["operations", "Operations"],
    ["roles", "Roles"],
  ])("opens the saved table address %s", async (path, table) => {
    const data = fixture();
    const session = identity(true);
    window.history.pushState({}, "", `/${CONTROL}/projects/widgets/dev/${path}`);
    render(() => (
      <Shell
        title="WAMN"
        org="acme"
        sections={[]}
        controlSections={controlSections}
        fetch={(url, init) =>
          String(url).includes("/api/wamn_control/") && !String(url).endsWith("/control/mine")
            ? data.fetch(url, init)
            : session.fetch(url, init)
        }
      />
    ));
    expect((await screen.findByRole("table", { name: table })).closest("td")).toBeNull();
    expect(screen.getAllByRole("table")).toHaveLength(1);
    const reads = data.calls.filter((call) => call.includes("/environment/status")).length;
    fireEvent.click(screen.getByRole("button", { name: "Refresh results" }));
    await waitFor(() =>
      expect(data.calls.filter((call) => call.includes("/environment/status"))).toHaveLength(reads + 1),
    );
  });

  it.each([
    "workloads/missing",
    "roles/missing",
    "operations/acme%3Areceiving%2Frecord/reaches/acme/statements/record.sql/relations/missing/reads",
    "operations/missing",
    "operations/acme%3Areceiving%2Frecord/reaches/missing",
    "operations/acme%3Areceiving%2Frecord/reaches/acme/statements/missing.sql",
  ])("rejects removed reference routes: %s", async (path) => {
    const data = fixture();
    const session = identity(true);
    window.history.pushState({}, "", `/${CONTROL}/projects/widgets/dev/${path}`);
    render(() => (
      <Shell
        title="WAMN"
        org="acme"
        sections={[]}
        controlSections={controlSections}
        fetch={(url, init) =>
          String(url).includes("/api/wamn_control/") && !String(url).endsWith("/control/mine")
            ? data.fetch(url, init)
            : session.fetch(url, init)
        }
      />
    ));
    await screen.findByText("This address names no page.");
    expect(document.querySelector(".ui-dashboard, header, nav, form")).toBeNull();
    expect(document.querySelectorAll(".ui-entry-panel")).toHaveLength(1);
    expect(document.querySelector(".ui-entry-panel")?.getAttribute("data-size")).toBe("viewport");
    expect(screen.getAllByRole("heading")).toHaveLength(1);
    expect(screen.getAllByRole("link")).toHaveLength(1);
    expect(screen.getByRole("link", { name: "Go to audience home" }).getAttribute("href")).toBe(`/${CONTROL}`);
    expect(screen.queryByRole("table")).toBeNull();
    window.history.pushState({}, "", `/${CONTROL}/projects/widgets/dev/workloads`);
    window.dispatchEvent(new PopStateEvent("popstate"));
    await screen.findByRole("table", { name: "Workloads" });
    expect(document.querySelector(".ui-dashboard")).not.toBeNull();
    expect(screen.queryByText("This address names no page.")).toBeNull();
  });

  it("uses the standalone page when status confirms that an environment disappeared", async () => {
    const data = fixture();
    const session = identity(true);
    window.history.pushState({}, "", `/${CONTROL}/projects/widgets/dev`);
    render(() => (
      <Shell
        title="WAMN"
        org="acme"
        sections={[]}
        controlSections={controlSections}
        fetch={(url, init) => {
          if (String(url).includes("/environment/status"))
            return Promise.resolve(
              new Response(JSON.stringify([{ error: { code: "environment_not_found", detail: { field: "env" } } }])),
            );
          return String(url).includes("/api/wamn_control/") && !String(url).endsWith("/control/mine")
            ? data.fetch(url, init)
            : session.fetch(url, init);
        }}
      />
    ));
    await screen.findByRole("heading", { name: "No page here" });
    expect(document.querySelector(".ui-dashboard")).toBeNull();
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
    expect(screen.getByText("State: active")).toBeTruthy();
  });

  it("does not request a profile when no release is installed", async () => {
    const data = fixture("inactive", null);
    mount(data);
    await screen.findByRole("button", { name: "Activate environment" });
    expect(screen.getByRole("group", { name: "Release" }).getAttribute("data-state")).toBe("unknown");
    expect(screen.queryByRole("region", { name: "Release profile" })).toBeNull();
    expect(data.calls.some((path) => path.includes("/release/profile"))).toBe(false);
  });

  it.each(["operations", "roles"] as const)("shows an empty %s table without a release", async (table) => {
    const data = fixture("active", null);
    render(() => (
      <Environment
        transport={data.transport}
        holds={() => true}
        params={{ aud: CONTROL, project: "widgets", env: "dev" }}
        table={table}
        search={{}}
        open={() => {}}
        close={() => {}}
      />
    ));
    const records = await screen.findByRole("table", { name: table === "operations" ? "Operations" : "Roles" });
    expect(within(records).queryAllByRole("cell")).toHaveLength(0);
    expect(screen.getAllByRole("table")).toHaveLength(1);
    expect(data.calls.some((path) => path.includes("/release/profile"))).toBe(false);
  });

  it("lets a project admin read its environment but offers no status writes", async () => {
    const data = fixture();
    mount(data, false);
    await screen.findByRole("region", { name: "Release profile" });
    expect(screen.queryByRole("button", { name: /environment$/ })).toBeNull();
  });

  it.each(["", "/workloads", "/live-releases", "/operations", "/roles"])(
    "refuses an environment address outside the project admin's scope: %s",
    async (path) => {
      const session = identity(true, [], undefined, {
        org_admin: false,
        projects: [{ project: "widgets", project_admin: true }],
      });
      window.history.pushState({}, "", `/${CONTROL}/projects/other/dev${path}`);
      render(() => (
        <Shell title="WAMN" org="acme" sections={[]} controlSections={controlSections} fetch={session.fetch} />
      ));
      await screen.findByText("This address names no page.");
      expect(session.state.calls).toEqual(["/password/renew", "/api/wamn_control/control/mine"]);
    },
  );
});

const operationContracts = import.meta.glob<{
  fields: { path: string; nullable: boolean }[];
}>("../../../crates/catalog/model/src/host_route/control/contracts/{application,schema}/*.input.json", {
  eager: true,
  import: "default",
});
const statusContract = import.meta.glob<{ fields: { path: string }[] }>(
  "../../../crates/catalog/model/src/host_route/control/contracts/operation/status.input.json",
  { eager: true, import: "default" },
);
const releaseDigest = `sha256:${"a".repeat(64)}`;

function operationsFixture(options: { refuse?: boolean; states?: string[]; project?: string; admin?: boolean } = {}) {
  const base = fixture();
  const writes: { route: string; body: { request_id: string; value: Record<string, unknown> }[] }[] = [];
  const reads: { params: URLSearchParams; signal: AbortSignal | null | undefined }[] = [];
  const states = [...(options.states ?? ["succeeded"])];
  const fetch: typeof globalThis.fetch = async (url, init) => {
    const address = new URL(String(url), "http://localhost");
    if (/\/(application|schema)\//.test(address.pathname)) {
      const body = JSON.parse(String(init?.body));
      writes.push({ route: address.pathname, body });
      return new Response(
        JSON.stringify([
          options.refuse
            ? { request_id: body[0].request_id, error: { code: "permission_denied", detail: { operation: "deploy" } } }
            : { request_id: body[0].request_id, value: { job: "op-job" } },
        ]),
      );
    }
    if (address.pathname.endsWith("/operation/status")) {
      reads.push({ params: address.searchParams, signal: init?.signal });
      const state = states.length > 1 ? states.shift()! : states[0];
      return new Response(JSON.stringify([{ value: { job: "op-job", state, log: `worker ${state}` } }]));
    }
    return base.fetch(url, init);
  };
  const mounted = render(() => (
    <Environment
      transport={createTransport({ baseUrl: "/api", fetch })}
      holds={controlHolds({
        orgAdmin: false,
        projects: [{ project: options.project ?? "widgets", projectAdmin: options.admin ?? true }],
      })}
      params={{ aud: CONTROL, project: "widgets", env: "dev" }}
      search={{}}
      open={() => {}}
      close={() => {}}
    />
  ));
  return { writes, reads, ...mounted };
}

async function openOperation(action: string, withRelease = true) {
  fireEvent.click(await screen.findByRole("button", { name: new RegExp(`^${action}\\b`, "i") }));
  await screen.findByRole("button", { name: /^Start / });
  const application = screen.queryByRole("textbox", { name: "Application" });
  if (application) input(application, "receiving");
  const release = screen.queryByRole("textbox", { name: /Release digest/ });
  if (release && withRelease) input(release, releaseDigest);
  fireEvent.click(screen.getByRole("button", { name: /^Start / }));
}

describe("environment operation controls", () => {
  for (const [path, contract] of Object.entries(operationContracts)) {
    const [family, filename] = path.split("/").slice(-2);
    const action = filename!.split(".")[0]!;
    const release = contract.fields.find((field) => field.path === "value.release");
    for (const withRelease of release?.nullable ? [true, false] : [true]) {
      it(`sends the contract fields for ${family}.${action}, explicit release=${withRelease}`, async () => {
        const data = operationsFixture();
        await openOperation(action, withRelease);
        if (["reset", "apply", "rollback"].includes(action)) {
          const dialog = within(await screen.findByRole("alertdialog"));
          expect(data.writes).toEqual([]);
          expect(dialog.getByText(/Project: widgets; environment: dev; application:/).textContent).toContain(
            action === "reset" ? "all applications" : "receiving",
          );
          fireEvent.click(dialog.getByRole("button", { name: /^Confirm / }));
        }
        await screen.findByText("State: succeeded");
        const write = data.writes[0]!;
        expect(write.route).toBe(`/api/wamn_control/${family}/${action}`);
        expect(Object.keys(write.body[0]!).sort()).toEqual(
          [...new Set(contract.fields.map((field) => field.path.split(".")[0]))].sort(),
        );
        expect(write.body[0]!.request_id).toBeTruthy();
        const expectedValues: Record<string, unknown> = {
          project: "widgets",
          env: "dev",
          application: action === "adopt" ? null : "receiving",
          release: withRelease ? releaseDigest : null,
          resolve: null,
        };
        expect(write.body[0]!.value).toEqual(
          Object.fromEntries(
            contract.fields
              .filter((field) => field.path.startsWith("value."))
              .map((field) => {
                const name = field.path.slice("value.".length);
                return [name, expectedValues[name]];
              }),
          ),
        );
        expect(screen.getByLabelText("Log tail").textContent).toBe("worker succeeded");
        expect([...data.reads[0]!.params.keys()].sort()).toEqual(
          Object.values(statusContract)
            .flatMap((entry) => entry.fields.map((field) => field.path))
            .sort(),
        );
        expect(Object.fromEntries([...data.reads[0]!.params].map(([key, value]) => [key, JSON.parse(value)]))).toEqual({
          project: "widgets",
          env: "dev",
          job: "op-job",
        });
      });
    }
  }

  it("requires reset confirmation and cancellation sends nothing", async () => {
    const data = operationsFixture();
    await openOperation("reset");
    const dialog = within(await screen.findByRole("alertdialog"));
    expect(dialog.getByText(/All applications lose their data/)).toBeTruthy();
    expect(data.writes).toEqual([]);
    fireEvent.click(dialog.getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(screen.queryByRole("alertdialog")).toBeNull());
    expect(data.writes).toEqual([]);
    expect(data.reads).toEqual([]);
  });

  it("shows a refusal code without polling a nonexistent Job", async () => {
    const data = operationsFixture({ refuse: true });
    await openOperation("deploy");
    expect((await screen.findByRole("alert")).textContent).toContain("permission_denied");
    expect(data.reads).toEqual([]);
  });

  it.each([{ project: "other" }, { admin: false }])(
    "hides controls without authority for the target: %j",
    async (options) => {
      operationsFixture(options);
      await screen.findByRole("region", { name: "Release profile" });
      expect(screen.queryByRole("region", { name: "Deployment controls" })).toBeNull();
    },
  );

  it.each(["succeeded", "failed"])("polls pending and running to %s, then stops", async (terminal) => {
    const data = operationsFixture({ states: ["pending", "running", terminal] });
    await openOperation("deploy");
    await screen.findByText("worker pending");
    await screen.findByText("worker running", {}, { timeout: 3000 });
    await screen.findByText(`worker ${terminal}`, {}, { timeout: 3000 });
    const reads = data.reads.length;
    await new Promise((resolve) => setTimeout(resolve, 1100));
    expect(data.reads.length).toBe(reads);
  });

  it("aborts status reads and clears polling when leaving the page", async () => {
    const data = operationsFixture({ states: ["running"] });
    await openOperation("deploy");
    await screen.findByText("worker running");
    const signal = data.reads[0]!.signal;
    const reads = data.reads.length;
    data.unmount();
    expect(signal?.aborted).toBe(true);
    await new Promise((resolve) => setTimeout(resolve, 1100));
    expect(data.reads.length).toBe(reads);
  });
});
