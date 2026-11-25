import { fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { describe, expect, it } from "vitest";
import { createTransport } from "@wamn/web-runtime";
import applicationOpenApi from "../../target/wamn/platform_ui/application.openapi.json";
import controlOpenApi from "../../target/wamn/platform_ui/control.openapi.json";
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
              users_with_role: 3,
              roles: 4,
              memberships: 5,
              rollout: { replicas: 2, updated_replicas: 1, ready_replicas: 1, available_replicas: 1 },
              workloads: digest
                ? [
                    {
                      name: "http",
                      application: "orders",
                      package_digest: digest,
                      revision: 2,
                      conditions: [{ type: "Ready", status: "True" }],
                    },
                    {
                      name: "worker",
                      application: "receiving",
                      package_digest: "sha256:other",
                      revision: 1,
                      conditions: [],
                    },
                  ]
                : [],
            }
          : undefined;
    return new Response(JSON.stringify([{ value }]));
  };
  return { fetch, calls, writes, transport: createTransport({ baseUrl: "/api", fetch }) };
}

function mount(data: ReturnType<typeof fixture>, orgAdmin = true) {
  render(() => (
    <Environment
      transport={data.transport}
      holds={controlHolds({ org_admin: orgAdmin, projects: [{ project: "widgets", project_admin: true }] })}
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
        holds={controlHolds({ org_admin: false, projects: [{ project: "widgets", project_admin: true }] })}
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

  it("shows environment status", async () => {
    const data = fixture();
    mount(data);
    await waitFor(() =>
      expect(Number(screen.getByRole("group", { name: "Workloads" }).textContent?.match(/\d+/)?.[0])).toBe(1),
    );
    expect(data.calls.filter((path) => path.includes("/environment/status"))).toHaveLength(1);
    expect(Number(screen.getByRole("group", { name: "Roles" }).textContent?.match(/\d+/)?.[0])).toBe(4);
  });

  it("expands workload details without changing the address", async () => {
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
    {
      const observed = conditions.getAllByRole("cell").map((cell) => cell.textContent);
      expect(observed).toHaveProperty(["length"], 2);
      expect(observed).toHaveProperty(["0"], "Ready");
      expect(observed).toHaveProperty(["1"], "True");
    }
    expect(window.location.pathname).toBe(`${base}/workloads`);
    expect(screen.getByRole("table", { name: "Conditions for http" }).closest("td")).not.toBeNull();
    expect(screen.getAllByRole("button", { name: "Refresh results" })).toHaveLength(1);
    fireEvent.click(screen.getByRole("button", { name: "Collapse details for row http" }));
    await waitFor(() => expect(screen.queryByRole("table", { name: "Conditions for http" })).toBeNull());
  });

  it.each(["workloads", "live-releases"])("opens the saved table address %s", async (path) => {
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
    await screen.findByRole("table");
    expect(screen.getAllByRole("table")).toHaveLength(1);
    const reads = data.calls.filter((call) => call.includes("/environment/status")).length;
    fireEvent.click(screen.getByRole("button", { name: "Refresh results" }));
    await waitFor(() =>
      expect(data.calls.filter((call) => call.includes("/environment/status"))).toHaveLength(reads + 1),
    );
  });

  it.each(["workloads/missing", "roles/missing", "operations/missing"])(
    "rejects removed reference routes: %s",
    async (path) => {
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
      await screen.findByRole("link", { name: "Go to audience home" });
      expect(screen.getAllByRole("heading")).toHaveLength(1);
      expect(screen.getAllByRole("link")).toHaveLength(1);
      expect(screen.getByRole("link", { name: "Go to audience home" }).getAttribute("href")).toBe(`/${CONTROL}`);
      expect(screen.queryByRole("table")).toBeNull();
      window.history.pushState({}, "", `/${CONTROL}/projects/widgets/dev/workloads`);
      window.dispatchEvent(new PopStateEvent("popstate"));
      await screen.findByRole("table", { name: "Workloads" });
      expect(screen.queryByRole("link", { name: "Go to audience home" })).toBeNull();
    },
  );

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
      {
        const observed = data.writes;
        expect(observed).toHaveProperty(["length"], 1);
        expect(observed).toHaveProperty(["0", "project"], "widgets");
        expect(observed).toHaveProperty(["0", "env"], "dev");
      }
      expect(data.calls.filter((path) => path.includes("/environment/list"))).toHaveLength(2);
    },
  );

  it("keeps the current action and reports an unsuccessful write", async () => {
    const data = fixture("active", "sha256:release", true);
    mount(data);
    fireEvent.click(await screen.findByRole("button", { name: "Inactivate environment" }));
    await screen.findByRole("alert");
    expect(screen.queryByRole("button", { name: "Activate environment" })).toBeNull();
    expect(
      screen
        .getByText(/^State:/)
        .textContent?.split(":")[1]
        ?.trim(),
    ).toBe("active");
  });

  it("shows no workloads when no release is installed", async () => {
    const data = fixture("inactive", null);
    mount(data);
    await screen.findByRole("button", { name: "Activate environment" });
    expect(Number(screen.getByRole("group", { name: "Workloads" }).textContent?.match(/\d+/)?.[0])).toBe(0);
  });

  it("lets a project admin read its environment but offers no status writes", async () => {
    const data = fixture();
    mount(data, false);
    await screen.findByRole("region", { name: "Environment status" });
    expect(screen.queryByRole("button", { name: /environment$/ })).toBeNull();
  });

  it.each(["", "/workloads", "/live-releases"])(
    "refuses an environment address outside the project admin's view: %s",
    async (path) => {
      const session = identity(true, undefined, {
        org_admin: false,
        projects: [{ project: "widgets", project_admin: true }],
      });
      window.history.pushState({}, "", `/${CONTROL}/projects/other/dev${path}`);
      render(() => (
        <Shell title="WAMN" org="acme" sections={[]} controlSections={controlSections} fetch={session.fetch} />
      ));
      await screen.findByRole("link", { name: "Go to audience home" });
      {
        const observed = session.state.calls;
        expect(observed).toHaveProperty(["length"], 2);
        expect(observed).toHaveProperty(["0"], "/password/renew");
        expect(observed).toHaveProperty(["1"], "/api/wamn_control/control/mine");
      }
    },
  );
});

const paths = { ...applicationOpenApi.paths, ...controlOpenApi.paths };
const statusParameters = paths["/wamn_control/operation/status"].get.parameters
  .filter((parameter) => parameter.in === "query")
  .map((parameter) => parameter.name)
  .sort();
const releaseDigest = `sha256:${"a".repeat(64)}`;

function operationsFixture(options: { refuse?: boolean; states?: string[]; project?: string; admin?: boolean } = {}) {
  const base = fixture();
  const writes: { route: string; body: { request_id: string; value: Record<string, unknown> }[] }[] = [];
  const reads: { params: URLSearchParams; signal: AbortSignal | null | undefined }[] = [];
  const statuses: { job: string; state: string | undefined; log: string }[] = [];
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
      const status = { job: "op-job", state, log: `worker ${state}` };
      statuses.push(status);
      return new Response(JSON.stringify([{ value: status }]));
    }
    return base.fetch(url, init);
  };
  const mounted = render(() => (
    <Environment
      transport={createTransport({ baseUrl: "/api", fetch })}
      holds={controlHolds({
        org_admin: false,
        projects: [{ project: options.project ?? "widgets", project_admin: options.admin ?? true }],
      })}
      params={{ aud: CONTROL, project: "widgets", env: "dev" }}
      search={{}}
      open={() => {}}
      close={() => {}}
    />
  ));
  return { writes, reads, statuses, ...mounted };
}

async function openOperation(action: string, withTarget = true) {
  fireEvent.click(await screen.findByRole("button", { name: new RegExp(`^${action}\\b`, "i") }));
  await screen.findByRole("button", { name: /^Start / });
  const application = screen.queryByRole("textbox", { name: "Application" });
  if (application) input(application, "receiving");
  const release = screen.queryByRole("textbox", { name: /Release digest/ });
  if (release && withTarget) input(release, releaseDigest);
  const revision = screen.queryByRole("textbox", { name: /Revision/ });
  if (revision && withTarget) input(revision, "2");
  fireEvent.click(screen.getByRole("button", { name: /^Start / }));
}

describe("environment operation controls", () => {
  for (const [path, operation] of Object.entries(paths)) {
    const [, family, action] = /^\/wamn_control\/(application|schema)\/([^/]+)$/.exec(path) ?? [];
    if (!family || !action) continue;
    if (!("post" in operation)) throw new Error(`Missing POST contract for ${path}`);
    const { properties } = operation.post.requestBody.content["application/json"].schema.items;
    if (!("value" in properties)) throw new Error(`Missing request value schema for ${path}`);
    const valueProperties = properties.value.properties;
    const fields = Object.keys(valueProperties).sort();
    const target = Object.entries(valueProperties).find(([name]) => name === "release" || name === "revision")?.[1];
    const nullableTarget = target && "type" in target && Array.isArray(target.type) && target.type.includes("null");
    for (const withTarget of nullableTarget ? [true, false] : [true]) {
      it(`sends the contract fields for ${family}.${action}, explicit target=${withTarget}`, async () => {
        const data = operationsFixture();
        await openOperation(action, withTarget);
        if (["reset", "apply", "rollback"].includes(action)) {
          const dialog = within(await screen.findByRole("alertdialog"));
          {
            const observed = data.writes;
            expect(observed).toHaveProperty(["length"], 0);
          }
          const scope = dialog.getByText((text) => text.includes("widgets") && text.includes("dev"));
          if (fields.includes("application")) expect(scope.textContent).toContain("receiving");
          fireEvent.click(dialog.getByRole("button", { name: /^Confirm / }));
        }
        await waitFor(() => expect(screen.getByRole("status").textContent).toContain(data.statuses.at(-1)!.state!));
        const write = data.writes[0]!;
        expect(write.route).toBe(`/api${path}`);
        expect(Object.keys(write.body[0]!).sort()).toEqual(Object.keys(properties).sort());
        expect(write.body[0]!.request_id).toBeTruthy();
        const expectedValues: Record<string, unknown> = {
          project: "widgets",
          env: "dev",
          application: "receiving",
          release: withTarget ? releaseDigest : null,
          revision: withTarget ? 2 : null,
        };
        expect(Object.keys(write.body[0]!.value).sort()).toEqual(fields);
        for (const name of fields) expect(write.body[0]!.value[name]).toEqual(expectedValues[name]);
        expect(screen.getByLabelText("Log tail").textContent).toBe(data.statuses.at(-1)!.log);
        expect([...data.reads[0]!.params.keys()].sort()).toEqual(statusParameters);
        {
          const observed = Object.fromEntries(
            [...data.reads[0]!.params].map(([key, value]) => [key, JSON.parse(value)]),
          );
          expect(observed).toHaveProperty(["project"], "widgets");
          expect(observed).toHaveProperty(["env"], "dev");
          expect(observed).toHaveProperty(["job"], "op-job");
        }
      });
    }
  }

  it("requires reset confirmation and cancellation sends nothing", async () => {
    const data = operationsFixture();
    await openOperation("reset");
    const dialog = within(await screen.findByRole("alertdialog"));
    expect(dialog.getByRole("button", { name: "Confirm reset schema" })).toBeTruthy();
    const scope = dialog.getByText((text) => text.includes("widgets") && text.includes("dev"));
    expect(scope).toBeTruthy();
    {
      const observed = data.writes;
      expect(observed).toHaveProperty(["length"], 0);
    }
    fireEvent.click(dialog.getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(screen.queryByRole("alertdialog")).toBeNull());
    {
      const observed = data.writes;
      expect(observed).toHaveProperty(["length"], 0);
    }
    {
      const observed = data.reads;
      expect(observed).toHaveProperty(["length"], 0);
    }
  });

  it("shows a refusal code without polling a nonexistent Job", async () => {
    const data = operationsFixture({ refuse: true });
    await openOperation("deploy");
    expect(await screen.findByRole("alert")).toBeTruthy();
    {
      const observed = data.reads;
      expect(observed).toHaveProperty(["length"], 0);
    }
  });

  it.each([{ project: "other" }, { admin: false }])(
    "hides controls without authority for the target: %j",
    async (options) => {
      operationsFixture(options);
      await screen.findByRole("region", { name: "Environment status" });
      expect(screen.queryByRole("region", { name: "Deployment controls" })).toBeNull();
    },
  );

  it.each(["succeeded", "failed"])("polls pending and running to %s, then stops", async (terminal) => {
    const states = ["pending", "running", terminal];
    const data = operationsFixture({ states });
    await openOperation("deploy");
    for (const state of states) {
      await waitFor(
        () => {
          const observed = data.statuses.find((status) => status.state === state);
          expect(observed).toBeDefined();
          expect(screen.getByLabelText("Log tail").textContent).toBe(observed!.log);
          expect(screen.getByRole("status").textContent).toContain(state);
        },
        { timeout: 3000 },
      );
    }
    const reads = data.reads.length;
    await new Promise((resolve) => setTimeout(resolve, 1100));
    expect(data.reads.length).toBe(reads);
  });

  it("aborts status reads and clears polling when leaving the page", async () => {
    const data = operationsFixture({ states: ["running"] });
    await openOperation("deploy");
    await waitFor(() => {
      expect(data.statuses).toHaveLength(1);
      expect(screen.getByLabelText("Log tail").textContent).toBe(data.statuses[0]!.log);
    });
    const signal = data.reads[0]!.signal;
    const reads = data.reads.length;
    data.unmount();
    expect(signal?.aborted).toBe(true);
    await new Promise((resolve) => setTimeout(resolve, 1100));
    expect(data.reads.length).toBe(reads);
  });
});
