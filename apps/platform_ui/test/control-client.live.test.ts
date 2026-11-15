// @vitest-environment node
/**
 * The application administration routes through their generated client
 * (docs/plan/platform-ui.md §4.6).
 *
 * `control_client_live` in `crates/execution/host/tests` serves the routes over HTTP
 * and runs this file with the endpoint and two session tokens. Without them
 * it skips.
 */
import { describe, expect, test } from "vitest";

import { createTransport } from "@wamn/web-runtime";
import { recordTransport } from "./record-transport";
import { permission, role, user, userRole } from "../src/generated/wamn_control/index.js";

// The package has no Node types, and the test reads only its environment.
declare const process: { readonly env: { readonly [name: string]: string | undefined } };

const baseUrl = process.env["WAMN_CONTROL_CLIENT_BASE_URL"];
const admin = process.env["WAMN_CONTROL_CLIENT_ADMIN_TOKEN"];
const adminId = process.env["WAMN_CONTROL_CLIENT_ADMIN_ID"] ?? "";
const member = process.env["WAMN_CONTROL_CLIENT_MEMBER_TOKEN"];
const memberId = process.env["WAMN_CONTROL_CLIENT_MEMBER_ID"] ?? "";

const READ = "session-test:purchase/read";
const WRITE = "session-test:purchase/write";

describe.skipIf(baseUrl === undefined)("the generated control client", () => {
  const as = (credential: string | undefined) => createTransport({ baseUrl: baseUrl ?? "", credential });
  let request = 0;
  const requestId = () => `control-client-${(request += 1)}`;

  test("refuses a caller without admin", async () => {
    const refused = await role.list(as(member), [{}]);
    {
      const observed = refused;
      expect(observed).toHaveProperty(["status"], "refused");
      expect(observed).toHaveProperty(["code"], "permission-denied");
    }
    {
      const observed = await permission.mine(as(member), [{}]);
      expect(observed).toHaveProperty(["status"], "completed");
      expect(observed).toHaveProperty(["value", "admin"], false);
      expect(observed).toHaveProperty(["value", "permissions", "length"], 1);
      expect(observed).toHaveProperty(["value", "permissions", "0"], READ);
    }
  });

  test("answers and refuses as the contracts declare", async () => {
    const recorded = recordTransport(baseUrl ?? "", admin);
    const transport = recorded.transport;
    {
      const observed = await user.list(transport, [{}]);
      expect(observed).toHaveProperty(["status"], "completed");
      const wire = recorded.value();
      const rows = wire["users"] as Record<string, unknown>[];
      expect(observed).toHaveProperty(["value", "users", "length"], rows.length);
      rows.forEach((row, index) => expect(observed).toHaveProperty(["value", "users", String(index), "id"], row["id"]));
      rows.forEach((row, index) =>
        expect(observed).toHaveProperty(["value", "users", String(index), "email"], row["email"]),
      );
      rows.forEach((row, index) =>
        expect(observed).toHaveProperty(["value", "users", String(index), "displayName"], row["display_name"]),
      );
      rows.forEach((row, index) =>
        expect(observed).toHaveProperty(["value", "users", String(index), "roles"], row["roles"]),
      );
      rows.forEach((row, index) =>
        expect(observed).toHaveProperty(["value", "users", String(index), "adminCovered"], row["admin_covered"]),
      );
      for (const [wireName, member] of [
        ["total", "total"],
        ["next_cursor", "nextCursor"],
      ] as const) {
        if (wireName in wire) expect(observed).toHaveProperty(["value", member], wire[wireName]);
      }
    }
    {
      const observed = await role.create(transport, [{ requestId: requestId(), value: { role: "clerk" } }]);
      expect(observed).toHaveProperty(["status"], "completed");
      expect(observed).toHaveProperty(["value", "created"], true);
    }
    {
      const observed = await role.list(transport, [{}]);
      expect(observed).toHaveProperty(["status"], "completed");
      const wire = recorded.value();
      const rows = wire["roles"] as Record<string, unknown>[];
      expect(observed).toHaveProperty(["value", "roles", "length"], rows.length);
      expect(observed).toHaveProperty(["value", "roles"], wire.roles);
      for (const [wireName, member] of [
        ["total", "total"],
        ["next_cursor", "nextCursor"],
      ] as const) {
        if (wireName in wire) expect(observed).toHaveProperty(["value", member], wire[wireName]);
      }
    }
    {
      const observed = await role.delete_(transport, [{ requestId: requestId(), value: { role: "admin" } }]);
      expect(observed).toHaveProperty(["status"], "refused");
      expect(observed).toHaveProperty(["code"], "admin_fixed");
      expect(observed).toHaveProperty(["detail", "detail", "field"], "role");
    }

    const granted = await permission.grant(transport, [
      { requestId: requestId(), value: { role: "clerk", operation: WRITE } },
    ]);
    {
      const observed = granted;
      expect(observed).toHaveProperty(["status"], "completed");
      expect(observed).toHaveProperty(["value", "rowsAdded"], 1);
    }
    {
      const observed = await permission.grant(transport, [
        { requestId: requestId(), value: { role: "clerk", operation: READ } },
      ]);
      expect(observed).toHaveProperty(["status"], "completed");
      expect(observed).toHaveProperty(["value", "rowsAdded"], 1);
    }
    {
      const observed = await permission.grant(transport, [
        { requestId: requestId(), value: { role: "clerk", operation: "wamn-control:role/list" } },
      ]);
      expect(observed).toHaveProperty(["status"], "refused");
      expect(observed).toHaveProperty(["code"], "operation_not_grantable");
      expect(observed).toHaveProperty(["detail", "detail", "field"], "operation");
    }

    const listed = await permission.list(transport, [{ role: "clerk" }]);
    expect(listed.status).toBe("completed");
    const operations = listed.status === "completed" ? listed.value.operations : [];
    {
      const observed = operations.find((entry) => entry.operation === READ);
      expect(observed).toHaveProperty(["operation"], READ);
      expect(observed).toHaveProperty(["served"], true);
      expect(observed).toHaveProperty(["grantable"], true);
      expect(observed).toHaveProperty(["adminOnly"], false);
      expect(observed).toHaveProperty(["selected"], true);
    }
    {
      const observed = operations.find((entry) => entry.operation === "wamn-control:role/list");
      expect(observed).toHaveProperty(["grantable"], false);
      expect(observed).toHaveProperty(["adminOnly"], true);
    }

    {
      const observed = await permission.revoke(transport, [
        { requestId: requestId(), value: { role: "clerk", operation: READ } },
      ]);
      expect(observed).toHaveProperty(["status"], "completed");
      expect(observed).toHaveProperty(["value"], {});
    }
    {
      const observed = await permission.revoke(transport, [
        { requestId: requestId(), value: { role: "clerk", operation: READ } },
      ]);
      expect(observed).toHaveProperty(["status"], "refused");
      expect(observed).toHaveProperty(["code"], "permission_not_held");
      expect(observed).toHaveProperty(["detail", "detail", "field"], "operation");
    }

    {
      const observed = await userRole.grant(transport, [
        { requestId: requestId(), value: { userId: memberId, role: "clerk" } },
      ]);
      expect(observed).toHaveProperty(["status"], "completed");
      expect(observed).toHaveProperty(["value", "granted"], true);
    }
    {
      const observed = await userRole.revoke(transport, [
        { requestId: requestId(), value: { userId: adminId, role: "admin" } },
      ]);
      expect(observed).toHaveProperty(["status"], "refused");
      expect(observed).toHaveProperty(["code"], "admin_covered");
      expect(observed).toHaveProperty(["detail", "detail", "field"], "user_id");
    }
    {
      const observed = await userRole.revoke(transport, [
        { requestId: requestId(), value: { userId: memberId, role: "clerk" } },
      ]);
      expect(observed).toHaveProperty(["status"], "completed");
      expect(observed).toHaveProperty(["value", "revoked"], true);
    }
  });
});
