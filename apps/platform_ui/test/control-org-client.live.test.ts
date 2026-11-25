// @vitest-environment node
/**
 * The org and project routes of the control serving root through their
 * generated client (docs/plan/platform-ui.md §4.4 and §4.5).
 *
 * `control_client_live` in `crates/execution-host/tests` serves the routes over HTTP
 * and runs this file with the endpoint, two session tokens and the user ids.
 * Without them it skips.
 */
import { describe, expect, test } from "vitest";

import { recordTransport } from "./record-transport";
import { control, member, orgAdmin, project, projectAdmin, user } from "../src/control-client";

// The package has no Node types, and the test reads only its environment.
declare const process: { readonly env: { readonly [name: string]: string | undefined } };

const env = (name: string) => process.env[`WAMN_CONTROL_ORG_CLIENT_${name}`];
const baseUrl = env("BASE_URL");
const boss = env("BOSS_TOKEN");
const cat = env("CAT_TOKEN");
const bossId = env("BOSS_ID") ?? "";
const annId = env("ANN_ID") ?? "";
const danId = env("DAN_ID") ?? "";

describe.skipIf(baseUrl === undefined)("the generated control org client", () => {
  let request = 0;
  const requestId = () => `control-org-client-${(request += 1)}`;

  test("admits a project admin to its project only", async () => {
    const recorded = recordTransport(baseUrl ?? "", cat);
    const transport = recorded.transport;
    {
      const observed = await control.mine(transport, [{}]);
      expect(observed).toHaveProperty(["status"], "completed");
      const wire = recorded.value();
      const rows = wire["projects"] as Record<string, unknown>[];
      expect(observed).toHaveProperty(["value", "projects", "length"], rows.length);
      rows.forEach((row, index) =>
        expect(observed).toHaveProperty(["value", "projects", String(index), "project"], row["project"]),
      );
      rows.forEach((row, index) =>
        expect(observed).toHaveProperty(["value", "projects", String(index), "project_admin"], row["project_admin"]),
      );
      expect(observed).toHaveProperty(["value", "org_admin"], wire.org_admin);
      for (const [wireName, member] of [
        ["total", "total"],
        ["next_cursor", "next_cursor"],
      ] as const) {
        if (wireName in wire) expect(observed).toHaveProperty(["value", member], wire[wireName]);
      }
    }
    {
      const observed = await user.list(transport, [{}]);
      expect(observed).toHaveProperty(["status"], "completed");
      const wire = recorded.value();
      const rows = wire["users"] as Record<string, unknown>[];
      expect(observed).toHaveProperty(["value", "users", "length"], rows.length);
      rows.forEach((row, index) =>
        expect(observed).toHaveProperty(["value", "users", String(index), "principal_id"], row["principal_id"]),
      );
      rows.forEach((row, index) =>
        expect(observed).toHaveProperty(["value", "users", String(index), "email"], row["email"]),
      );
      rows.forEach((row, index) =>
        expect(observed).toHaveProperty(["value", "users", String(index), "display_name"], row["display_name"]),
      );
      rows.forEach((row, index) =>
        expect(observed).toHaveProperty(["value", "users", String(index), "status"], row["status"]),
      );
      rows.forEach((row, index) =>
        expect(observed).toHaveProperty(["value", "users", String(index), "org_admin"], row["org_admin"]),
      );
      for (const [wireName, member] of [
        ["total", "total"],
        ["next_cursor", "next_cursor"],
      ] as const) {
        if (wireName in wire) expect(observed).toHaveProperty(["value", member], wire[wireName]);
      }
    }
    {
      const observed = await project.list(transport, [{}]);
      expect(observed).toHaveProperty(["status"], "refused");
      expect(observed).toHaveProperty(["code"], "permission-denied");
    }
    {
      const observed = await member.list(transport, [{ project: "billing" }]);
      expect(observed).toHaveProperty(["status"], "completed");
      const wire = recorded.value();
      const rows = wire["members"] as Record<string, unknown>[];
      expect(observed).toHaveProperty(["value", "members", "length"], rows.length);
      rows.forEach((row, index) =>
        expect(observed).toHaveProperty(["value", "members", String(index), "principal_id"], row["principal_id"]),
      );
      rows.forEach((row, index) =>
        expect(observed).toHaveProperty(["value", "members", String(index), "org_admin"], row["org_admin"]),
      );
      rows.forEach((row, index) =>
        expect(observed).toHaveProperty(["value", "members", String(index), "project_admin"], row["project_admin"]),
      );
      rows.forEach((row, index) =>
        expect(observed).toHaveProperty(["value", "members", String(index), "environments"], row["environments"]),
      );
      for (const [wireName, member] of [
        ["total", "total"],
        ["next_cursor", "next_cursor"],
      ] as const) {
        if (wireName in wire) expect(observed).toHaveProperty(["value", member], wire[wireName]);
      }
    }
  });

  test("answers and refuses as the contracts declare", async () => {
    const recorded = recordTransport(baseUrl ?? "", boss);
    const transport = recorded.transport;
    {
      const observed = await user.list(transport, [{}]);
      expect(observed).toHaveProperty(["status"], "completed");
      const wire = recorded.value();
      const rows = wire["users"] as Record<string, unknown>[];
      expect(observed).toHaveProperty(["value", "users", "length"], rows.length);
      rows.forEach((row, index) =>
        expect(observed).toHaveProperty(["value", "users", String(index), "principal_id"], row["principal_id"]),
      );
      rows.forEach((row, index) =>
        expect(observed).toHaveProperty(["value", "users", String(index), "email"], row["email"]),
      );
      rows.forEach((row, index) =>
        expect(observed).toHaveProperty(["value", "users", String(index), "display_name"], row["display_name"]),
      );
      rows.forEach((row, index) =>
        expect(observed).toHaveProperty(["value", "users", String(index), "status"], row["status"]),
      );
      rows.forEach((row, index) =>
        expect(observed).toHaveProperty(["value", "users", String(index), "org_admin"], row["org_admin"]),
      );
      for (const [wireName, member] of [
        ["total", "total"],
        ["next_cursor", "next_cursor"],
      ] as const) {
        if (wireName in wire) expect(observed).toHaveProperty(["value", member], wire[wireName]);
      }
    }
    {
      const observed = await project.list(transport, [{}]);
      expect(observed).toHaveProperty(["status"], "completed");
      const wire = recorded.value();
      const rows = wire["projects"] as Record<string, unknown>[];
      expect(observed).toHaveProperty(["value", "projects", "length"], rows.length);
      rows.forEach((row, index) =>
        expect(observed).toHaveProperty(["value", "projects", String(index), "name"], row["name"]),
      );
      rows.forEach((row, index) =>
        expect(observed).toHaveProperty(["value", "projects", String(index), "state"], row["state"]),
      );
      for (const [wireName, member] of [
        ["total", "total"],
        ["next_cursor", "next_cursor"],
      ] as const) {
        if (wireName in wire) expect(observed).toHaveProperty(["value", member], wire[wireName]);
      }
    }
    {
      const observed = await user.activate(transport, [{ request_id: requestId(), value: { principal_id: annId } }]);
      expect(observed).toHaveProperty(["status"], "completed");
      expect(observed).toHaveProperty(["value", "principal_id"], annId);
      expect(observed).toHaveProperty(["value", "status"], "active");
    }
    {
      const observed = await user.activate(transport, [{ request_id: requestId(), value: { principal_id: danId } }]);
      expect(observed).toHaveProperty(["status"], "refused");
      expect(observed).toHaveProperty(["code"], "user_not_found");
      expect(observed).toHaveProperty(["detail", "detail", "field"], "principal_id");
    }
    {
      const observed = await orgAdmin.grant(transport, [{ request_id: requestId(), value: { principal_id: danId } }]);
      expect(observed).toHaveProperty(["status"], "refused");
      expect(observed).toHaveProperty(["code"], "user_not_active");
      expect(observed).toHaveProperty(["detail", "detail", "field"], "principal_id");
    }
    {
      const observed = await member.grant(transport, [
        { request_id: requestId(), value: { project: "billing", env: "prod", principal_id: annId } },
      ]);
      expect(observed).toHaveProperty(["status"], "refused");
      expect(observed).toHaveProperty(["code"], "environment_not_found");
      expect(observed).toHaveProperty(["detail", "detail", "field"], "env");
    }
    {
      const observed = await projectAdmin.grant(transport, [
        { request_id: requestId(), value: { project: "nowhere", principal_id: annId } },
      ]);
      expect(observed).toHaveProperty(["status"], "refused");
      expect(observed).toHaveProperty(["code"], "project_not_found");
      expect(observed).toHaveProperty(["detail", "detail", "field"], "project");
    }
    {
      const observed = await projectAdmin.revoke(transport, [
        { request_id: requestId(), value: { project: "billing", principal_id: bossId } },
      ]);
      expect(observed).toHaveProperty(["status"], "refused");
      expect(observed).toHaveProperty(["code"], "admin_covered");
      expect(observed).toHaveProperty(["detail", "detail", "field"], "principal_id");
    }
  });
});
