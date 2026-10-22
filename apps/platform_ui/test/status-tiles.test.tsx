import { render, screen, waitFor, within } from "@solidjs/testing-library";
import { describe, expect, it } from "vitest";
import type { environment } from "@wamn/control-org-client";
import { createTransport } from "@wamn/web-runtime";
import { EnvironmentTiles } from "../src/status-tiles";
import { ControlHome } from "../src/control-home";
import { CONTROL } from "./fixtures";

const status: environment.EnvironmentStatusResult = {
  digest: "sha256:123456789abcdef",
  rollout: { replicas: 2, updatedReplicas: 2, readyReplicas: 2, availableReplicas: 2 },
  usersWithRole: 3,
  roles: 4,
  memberships: 5,
  workloads: [{ name: "http", conditions: [{ type: "Ready", status: "True" }] }],
  live: [],
};

function tile(label: string, value: string, state: string, scope: Pick<typeof screen, "getByRole"> = screen) {
  const group = scope.getByRole("group", { name: label });
  expect(group.getAttribute("data-state")).toBe(state);
  expect(group.querySelector(".ui-status-tile-value")?.textContent).toBe(value);
  expect(group.querySelector("button, a")).toBeNull();
}

describe("Environment tiles", () => {
  it("shows a complete rollout, counts and ready workloads from one snapshot", () => {
    render(() => <EnvironmentTiles status={status} />);
    tile("Release", "12345678", "ok");
    tile("Users with a role", "3", "ok");
    tile("Roles", "4", "ok");
    tile("Memberships", "5", "ok");
    tile("Workloads", "1 ready / 0 not ready", "ok");
    expect(screen.getAllByRole("group")).toHaveLength(5);
  });

  it.each(["updatedReplicas", "readyReplicas", "availableReplicas"] as const)(
    "warns when %s is behind the rollout",
    (field) => {
      render(() => <EnvironmentTiles status={{ ...status, rollout: { ...status.rollout, [field]: 1 } }} />);
      tile("Release", "12345678", "warning");
    },
  );

  it("keeps missing release and counts unknown", () => {
    render(() => (
      <EnvironmentTiles status={{ ...status, digest: null, usersWithRole: null, roles: null, memberships: null }} />
    ));
    for (const label of ["Release", "Users with a role", "Roles", "Memberships", "Workloads"])
      tile(label, "Unknown", "unknown");
  });

  it("preserves measured zeros", () => {
    render(() => (
      <EnvironmentTiles status={{ ...status, usersWithRole: 0, roles: 0, memberships: 0, workloads: [] }} />
    ));
    for (const label of ["Users with a role", "Roles", "Memberships"]) tile(label, "0", "ok");
    tile("Workloads", "0 ready / 0 not ready", "ok");
  });

  it("warns for false, unknown and absent Ready conditions", () => {
    render(() => (
      <EnvironmentTiles
        status={{
          ...status,
          workloads: [
            ...status.workloads,
            { name: "false", conditions: [{ type: "Ready", status: "False" }] },
            { name: "unknown", conditions: [{ type: "Ready", status: "Unknown" }] },
            { name: "missing", conditions: [{ type: "Scheduled", status: "True" }] },
          ],
        }}
      />
    ));
    tile("Workloads", "1 ready / 3 not ready", "warning");
  });
});

describe("Control splash", () => {
  it.each([true, false])(
    "groups environments by project and isolates failed reads (org admin: %s)",
    async (orgAdmin) => {
      const calls: string[] = [];
      const opened: string[] = [];
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
                    projects: [
                      { name: "widgets", state: "active" },
                      { name: "gadgets", state: "active" },
                    ],
                  },
                },
              ]),
            );
          if (path.endsWith("/environment/list"))
            return new Response(
              JSON.stringify([
                {
                  value: {
                    environments: (input.project === "widgets" ? ["dev", "prod"] : ["dev"]).map((name) => ({
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
                  digest: status.digest,
                  rollout: { replicas: 2, updated_replicas: 2, ready_replicas: 2, available_replicas: 2 },
                  users_with_role: input.project === "widgets" ? 3 : 8,
                  roles: 4,
                  memberships: 5,
                  workloads: status.workloads,
                  live: [],
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
            orgAdmin,
            projects: [
              { project: "widgets", projectAdmin: true },
              { project: "gadgets", projectAdmin: true },
              { project: "unheld", projectAdmin: false },
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
      await waitFor(() => tile("Users with a role", "8", "ok", within(gadgets.getByRole("region", { name: "dev" }))));
      tile("Users with a role", "3", "ok", within(widgets.getByRole("region", { name: "dev" })));
      const failed = within(widgets.getByRole("region", { name: "prod" }));
      await failed.findAllByText(/status unavailable/);
      for (const label of ["Release", "Users with a role", "Roles", "Memberships", "Workloads"]) {
        tile(label, "Unknown", "unknown", failed);
        expect(failed.getByRole("group", { name: label }).textContent).toContain("status unavailable");
      }
      expect(screen.queryByRole("region", { name: "unheld" })).toBeNull();
      expect(widgets.getByRole("link", { name: "dev" }).getAttribute("href")).toBe(`/${CONTROL}/projects/widgets/dev`);
      widgets.getByRole("link", { name: "dev" }).click();
      expect(opened).toEqual(["projects/widgets/dev"]);
      expect(calls.filter((call) => call.includes("/environment/status:"))).toEqual([
        "/api/wamn_control/environment/status:widgets/dev",
        "/api/wamn_control/environment/status:widgets/prod",
        "/api/wamn_control/environment/status:gadgets/dev",
      ]);
      expect(calls.filter((call) => call.includes("/project/list:"))).toHaveLength(orgAdmin ? 1 : 0);
    },
  );
});
