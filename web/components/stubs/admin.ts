/**
 * One application's administration routes, held in memory, for the role grid
 * and the user grid.
 *
 * It answers `role.list`, `permission.list`, `permission.grant`,
 * `permission.revoke`, `user.list`, `user_role.grant` and `user_role.revoke`
 * as the host does, over a release of three grantable roots and two host
 * routes. A refusal goes through the runtime's classifier, so it carries the
 * text of its contract.
 */

import { classify, type JsonValue, type Outcome, type Transport, type WireRequest } from "@wamn/web-runtime";

/** The closure of each grantable root of the release. */
export const CLOSURES: Readonly<Record<string, readonly string[]>> = {
  "acme:receiving/record": ["acme:receiving/record", "acme:purchase/get"],
  "acme:purchase/get": ["acme:purchase/get"],
  "acme:inventory/query": ["acme:inventory/query"],
};

/** The host routes the release serves: one fixed to admin, one fixed to every member. */
const HOST: Readonly<Record<string, "admin" | "member">> = {
  "wamn-control:role/list": "admin",
  "wamn-control:permission/mine": "member",
};

export const ANN = "00000000-0000-4000-8000-00000000000a";
export const BOSS = "00000000-0000-4000-8000-00000000000b";

export interface AdminState {
  /** The roots each role selected. */
  readonly roots: Map<string, Set<string>>;
  /** The roles each user holds. */
  readonly held: Map<string, Set<string>>;
  /** The users that project-admin or org-admin covers. */
  readonly covered: Set<string>;
  /** The code the next write refuses with, or null. */
  refuseNext: string | null;
}

/** The starting state: `clerk` selected `receiving/record`, and boss holds a covered admin. */
export function adminState(): AdminState {
  return {
    roots: new Map([
      ["admin", new Set()],
      ["clerk", new Set(["acme:receiving/record"])],
    ]),
    held: new Map([
      [ANN, new Set(["clerk"])],
      [BOSS, new Set(["admin"])],
    ]),
    covered: new Set([BOSS]),
    refuseNext: null,
  };
}

const EMAIL: Readonly<Record<string, string>> = { [ANN]: "ann@example.test", [BOSS]: "boss@example.test" };

function refused(request: WireRequest, code: string, detail: JsonValue): Outcome<JsonValue> {
  return classify(request.contract, null, { status: 200, body: JSON.stringify([{ error: { code, detail } }]) });
}

const completed = (value: JsonValue): Outcome<JsonValue> => ({ status: "completed", value });

/** The roots of `role` whose closure holds `operation`, other than itself. */
function requiredBy(roots: ReadonlySet<string>, operation: string): string[] {
  return [...roots].filter((root) => root !== operation && (CLOSURES[root] ?? []).includes(operation)).sort();
}

function permissionList(state: AdminState, role: string): JsonValue {
  const roots = state.roots.get(role) ?? new Set<string>();
  const operations = [...Object.keys(CLOSURES), ...Object.keys(HOST)].sort();
  return {
    role,
    admin: role === "admin",
    operations: operations.map((operation) => ({
      operation,
      served: true,
      grantable: operation in CLOSURES,
      admin_only: HOST[operation] === "admin",
      selected: roots.has(operation),
      required_by: requiredBy(roots, operation),
    })),
  };
}

/** The transport over `state`, and every request it took. */
export function adminStub(state: AdminState = adminState()): { transport: Transport; sent: WireRequest[] } {
  const sent: WireRequest[] = [];
  const answer = (request: WireRequest): Outcome<JsonValue> => {
    const item = (request.items[0] ?? {}) as { [key: string]: JsonValue };
    const value = (item["value"] ?? {}) as { [key: string]: string };
    const route = request.operation.slice(0, request.operation.indexOf("@"));
    if (request.operation.includes("/grant@") || request.operation.includes("/revoke@")) {
      const code = state.refuseNext;
      if (code !== null) {
        state.refuseNext = null;
        return refused(request, code, { field: "operation" });
      }
    }
    switch (route) {
      case "wamn-control:role/list":
        return completed({ roles: [...state.roots.keys()].sort() });
      case "wamn-control:permission/list":
        return completed(permissionList(state, String(item["role"])));
      case "wamn-control:permission/grant": {
        const role = value["role"] ?? "";
        const operation = value["operation"] ?? "";
        if (role === "admin") {
          return refused(request, "admin_fixed", { field: "role" });
        }
        if (!(operation in CLOSURES)) {
          return refused(request, "operation_not_grantable", { field: "operation" });
        }
        state.roots.get(role)?.add(operation);
        return completed({ closure: [...(CLOSURES[operation] ?? [])], rows_added: 1 });
      }
      case "wamn-control:permission/revoke": {
        const roots = state.roots.get(value["role"] ?? "") ?? new Set<string>();
        const operation = value["operation"] ?? "";
        if (!roots.has(operation)) {
          return refused(request, "permission_not_selected", {
            field: "operation",
            required_by: requiredBy(roots, operation),
          });
        }
        roots.delete(operation);
        return completed({ still_required_by: requiredBy(roots, operation) });
      }
      case "wamn-control:user/list":
        return completed({
          users: [...state.held.keys()].sort().map((id) => ({
            id,
            email: EMAIL[id] ?? id,
            display_name: null,
            roles: [...(state.held.get(id) ?? [])].sort(),
            admin_covered: state.covered.has(id),
          })),
        });
      case "wamn-control:user-role/grant": {
        state.held.get(value["user_id"] ?? "")?.add(value["role"] ?? "");
        return completed({ granted: true });
      }
      case "wamn-control:user-role/revoke": {
        const userId = value["user_id"] ?? "";
        if (value["role"] === "admin" && state.covered.has(userId)) {
          return refused(request, "admin_covered", { field: "user_id" });
        }
        state.held.get(userId)?.delete(value["role"] ?? "");
        return completed({ revoked: true });
      }
      default:
        return refused(request, "permission_denied", { operation: request.operation });
    }
  };
  return {
    sent,
    transport: {
      invoke: (request: WireRequest) => {
        sent.push(request);
        return Promise.resolve(answer(request));
      },
    },
  };
}
