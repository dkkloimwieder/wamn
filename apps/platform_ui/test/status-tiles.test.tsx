import { cleanup, render, screen, waitFor, within } from "@solidjs/testing-library";
import { describe, expect, it } from "vitest";
import type { EnvStatusResult } from "../src/control-client";
import { createTransport } from "@wamn/web-runtime";
import { EnvironmentTiles } from "../src/status-tiles";
import { ControlHome } from "../src/control-home";
import { CONTROL } from "./fixtures";

const status: EnvStatusResult = {
  rollout: { replicas: 2, updated_replicas: 2, ready_replicas: 2, available_replicas: 2 },
  users_with_role: 3,
  roles: 4,
  memberships: 5,
  workloads: [
    {
      name: "http",
      app: "orders",
      package_digest: "sha256:release",
      revision: 2,
      conditions: [{ type: "Ready", status: "True" }],
    },
  ],
};

function tile(label: string, expected: readonly number[], view: Pick<typeof screen, "getByRole"> = screen) {
  const group = view.getByRole("group", { name: label });
  const measured = Array.from(group.textContent?.matchAll(/\d+/g) ?? [], ([value]) => Number(value));
  expect(measured).toEqual(expected);
  expect(within(group).queryByRole("button")).toBeNull();
  expect(within(group).queryByRole("link")).toBeNull();
}

describe("Environment tiles", () => {
  it("shows a complete rollout, counts and ready workloads from one snapshot", () => {
    render(() => <EnvironmentTiles status={status} />);
    tile("Host replicas", [2, 2]);
    tile("Users with a role", [3]);
    tile("Roles", [4]);
    tile("Memberships", [5]);
    tile("Workloads", [1, 0]);
  });

  it("keeps missing counters unknown without hiding measured workloads and replicas", () => {
    render(() => <EnvironmentTiles status={{ ...status, users_with_role: null, roles: null, memberships: null }} />);
    for (const label of ["Users with a role", "Roles", "Memberships"]) tile(label, []);
    tile("Workloads", [1, 0]);
    tile("Host replicas", [2, 2]);
    cleanup();
    render(() => <EnvironmentTiles />);
    for (const group of screen.getAllByRole("group")) {
      expect(Array.from(group.textContent?.matchAll(/\d+/g) ?? [])).toHaveLength(0);
    }
  });

  it("preserves measured zeros", () => {
    render(() => (
      <EnvironmentTiles status={{ ...status, users_with_role: 0, roles: 0, memberships: 0, workloads: [] }} />
    ));
    for (const label of ["Users with a role", "Roles", "Memberships"]) tile(label, [0]);
    tile("Workloads", [0, 0]);
  });

  it("warns for false, unknown and absent Ready conditions", () => {
    render(() => (
      <EnvironmentTiles
        status={{
          ...status,
          workloads: [
            ...status.workloads,
            { ...status.workloads[0]!, name: "false", conditions: [{ type: "Ready", status: "False" }] },
            { ...status.workloads[0]!, name: "unknown", conditions: [{ type: "Ready", status: "Unknown" }] },
            { ...status.workloads[0]!, name: "missing", conditions: [{ type: "Scheduled", status: "True" }] },
          ],
        }}
      />
    ));
    tile("Workloads", [1, 3]);
  });
});

describe("Control splash", () => {
  it.each([true, false])(
    "groups environments by project and isolates failed reads (org admin: %s)",
    async (orgAdmin) => {
      const calls: string[] = [];
      const opened: string[] = [];
      const envs = new Map([
        ["widgets", ["dev", "prod"]],
        ["gadgets", ["dev"]],
      ]);
      const transport = createTransport({
        baseUrl: "/api",
        fetch: async (url) => {
          const address = new URL(String(url), "http://localhost");
          const path = address.pathname;
          const input = Object.fromEntries([...address.searchParams].map(([name, value]) => [name, JSON.parse(value)]));
          calls.push(`${path}:${input.project ?? ""}/${input.env ?? ""}`);
          if (path.endsWith("/project/list"))
            return new Response(
              JSON.stringify([
                {
                  value: {
                    projects: [...envs.keys()].map((name) => ({ name, state: "active" })),
                  },
                },
              ]),
            );
          if (path.endsWith("/env/list"))
            return new Response(
              JSON.stringify([
                {
                  value: {
                    envs: (envs.get(input.project) ?? []).map((name) => ({
                      name,
                      state: "active",
                    })),
                  },
                },
              ]),
            );
          if (input.env === "prod") throw new Error("status unavailable");
          return new Response(
            JSON.stringify([
              {
                value: {
                  rollout: { replicas: 2, updated_replicas: 2, ready_replicas: 2, available_replicas: 2 },
                  users_with_role: input.project === "widgets" ? 3 : 8,
                  roles: 4,
                  memberships: 5,
                  workloads: status.workloads,
                },
              },
            ]),
          );
        },
      });
      render(() => (
        <ControlHome
          transport={transport}
          holds={() => true}
          controlMine={{
            org_admin: orgAdmin,
            projects: [
              { project: "widgets", project_admin: true },
              { project: "gadgets", project_admin: true },
              { project: "unheld", project_admin: false },
            ],
          }}
          params={{ aud: CONTROL }}
          search={{}}
          open={(path) => opened.push(path)}
          close={() => {}}
        />
      ));
      const widgets = within(await screen.findByRole("region", { name: "widgets" }));
      const gadgets = within(await screen.findByRole("region", { name: "gadgets" }));
      expect(widgets.getByRole("heading", { level: 2 }).textContent).toBe("widgets");
      expect(gadgets.getByRole("heading", { level: 2 }).textContent).toBe("gadgets");
      await waitFor(() => tile("Users with a role", [8], within(gadgets.getByRole("region", { name: "dev" }))));
      tile("Users with a role", [3], within(widgets.getByRole("region", { name: "dev" })));
      expect(widgets.getByRole("region", { name: "prod" })).toBeTruthy();
      expect(screen.queryByRole("region", { name: "unheld" })).toBeNull();
      expect(widgets.getByRole("link", { name: "dev" }).getAttribute("href")).toBe(`/${CONTROL}/projects/widgets/dev`);
      widgets.getByRole("link", { name: "dev" }).click();
      {
        const observed = opened;
        expect(observed).toHaveProperty(["length"], 1);
        expect(observed).toHaveProperty(["0"], "projects/widgets/dev");
      }
      expect(calls.filter((call) => call.includes("/env/status:")).sort()).toEqual(
        [...envs]
          .flatMap(([project, names]) => names.map((name) => `/api/wamn_control/env/status:${project}/${name}`))
          .sort(),
      );
      expect(calls.filter((call) => call.includes("/project/list:"))).toHaveLength(orgAdmin ? 1 : 0);
    },
  );
});
