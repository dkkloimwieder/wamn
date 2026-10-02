// @vitest-environment node
/**
 * The application administration routes through their generated client
 * (docs/plan/platform-ui.md §4.6).
 *
 * `control_client_live` in `tests/integration` serves the routes over HTTP
 * and runs this file with the endpoint and two session tokens. Without them
 * it skips.
 */
import { describe, expect, test } from "vitest";

import { createTransport } from "@wamn/web-runtime";
import { permission, role, user, userRole } from "@wamn/control-client";

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
    expect(refused).toMatchObject({ status: "refused", code: "permission-denied" });
    expect(await permission.mine(as(member), [{}])).toEqual({
      status: "completed",
      value: { admin: false, permissions: [READ] },
    });
  });

  test("answers and refuses as the contracts declare", async () => {
    const transport = as(admin);
    expect(await user.list(transport, [{}])).toMatchObject({
      status: "completed",
      value: {
        users: [
          { id: adminId, email: "first@example.test", displayName: null, roles: ["admin"] },
          { id: memberId, email: "member@example.test", displayName: null, roles: ["purchase-reader"] },
        ],
      },
    });
    expect(await role.create(transport, [{ requestId: requestId(), value: { role: "clerk" } }])).toEqual({
      status: "completed",
      value: { created: true },
    });
    expect(await role.list(transport, [{}])).toEqual({
      status: "completed",
      value: { roles: ["admin", "clerk", "purchase-reader"] },
    });
    expect(await role.delete_(transport, [{ requestId: requestId(), value: { role: "admin" } }])).toMatchObject({
      status: "refused",
      code: "admin_fixed",
      detail: { detail: { field: "role" } },
    });

    const granted = await permission.grant(transport, [
      { requestId: requestId(), value: { role: "clerk", operation: WRITE } },
    ]);
    expect(granted).toMatchObject({ status: "completed", value: { closure: [READ, WRITE] } });
    expect(
      await permission.grant(transport, [{ requestId: requestId(), value: { role: "clerk", operation: READ } }]),
    ).toMatchObject({ status: "completed", value: { closure: [READ] } });
    expect(
      await permission.grant(transport, [
        { requestId: requestId(), value: { role: "clerk", operation: "wamn-control:role/list" } },
      ]),
    ).toMatchObject({ status: "refused", code: "operation_not_grantable", detail: { detail: { field: "operation" } } });

    const listed = await permission.list(transport, [{ role: "clerk" }]);
    expect(listed.status).toBe("completed");
    const operations = listed.status === "completed" ? listed.value.operations : [];
    expect(operations.find((entry) => entry.operation === READ)).toEqual({
      operation: READ,
      served: true,
      grantable: true,
      adminOnly: false,
      selected: true,
      requiredBy: [WRITE],
    });
    expect(operations.find((entry) => entry.operation === "wamn-control:role/list")).toMatchObject({
      grantable: false,
      adminOnly: true,
    });

    expect(
      await permission.revoke(transport, [{ requestId: requestId(), value: { role: "clerk", operation: READ } }]),
    ).toEqual({ status: "completed", value: { stillRequiredBy: [WRITE] } });
    expect(
      await permission.revoke(transport, [{ requestId: requestId(), value: { role: "clerk", operation: READ } }]),
    ).toMatchObject({
      status: "refused",
      code: "permission_not_selected",
      detail: { detail: { field: "operation", required_by: [WRITE] } },
    });

    expect(
      await userRole.grant(transport, [{ requestId: requestId(), value: { userId: memberId, role: "clerk" } }]),
    ).toEqual({ status: "completed", value: { granted: true } });
    expect(
      await userRole.revoke(transport, [{ requestId: requestId(), value: { userId: adminId, role: "admin" } }]),
    ).toMatchObject({ status: "refused", code: "admin_covered", detail: { detail: { field: "user_id" } } });
    expect(
      await userRole.revoke(transport, [{ requestId: requestId(), value: { userId: memberId, role: "clerk" } }]),
    ).toEqual({ status: "completed", value: { revoked: true } });
  });
});
