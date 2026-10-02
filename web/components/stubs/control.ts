/**
 * One org's Control routes, held in memory, for the org screen and the
 * project screen.
 *
 * It answers `user.list`, `user.invite`, `user.activate`, `user.deactivate`,
 * `project.list`, `org_admin.grant`, `org_admin.revoke`, `environment.list`,
 * `member.list`, `member.grant`, `member.revoke`, `project_admin.grant` and
 * `project_admin.revoke` as the control host does, over the projects
 * `billing` and `shop` with the environment `dev` each. A refusal goes through
 * the runtime's classifier, so it carries the text of its contract.
 */

import { classify, type JsonValue, type Outcome, type Transport, type WireRequest } from "@wamn/web-runtime";

export const ANN = "00000000-0000-4000-8000-0000000000a1";
export const BOSS = "00000000-0000-4000-8000-0000000000b1";
export const CAT = "00000000-0000-4000-8000-0000000000c1";
const NEW = "00000000-0000-4000-8000-0000000000d1";

/** The environments of each project. */
const PROJECTS: Readonly<Record<string, readonly string[]>> = { billing: ["dev"], shop: ["dev"] };

export interface ControlMember {
  readonly email: string;
  readonly displayName: string;
  active: boolean;
}

export interface ControlState {
  /** The members of the org, by principal id. */
  readonly members: Map<string, ControlMember>;
  readonly orgAdmins: Set<string>;
  /** `project principal` for each `project-admin` row. */
  readonly projectAdmins: Set<string>;
  /** `project env principal` for each environment membership. */
  readonly memberships: Set<string>;
  /** The code the next write refuses with, and its detail, or null. */
  refuseNext: { readonly code: string; readonly detail: JsonValue } | null;
}

/** Boss holds org-admin, Cat project-admin of billing, and Ann a membership of billing dev. */
export function controlState(): ControlState {
  return {
    members: new Map([
      [ANN, { email: "ann@example.test", displayName: "Ann", active: true }],
      [BOSS, { email: "boss@example.test", displayName: "Boss", active: true }],
      [CAT, { email: "cat@example.test", displayName: "Cat", active: true }],
    ]),
    orgAdmins: new Set([BOSS]),
    projectAdmins: new Set([`billing ${BOSS}`, `shop ${BOSS}`, `billing ${CAT}`]),
    memberships: new Set([`billing dev ${BOSS}`, `shop dev ${BOSS}`, `billing dev ${CAT}`, `billing dev ${ANN}`]),
    refuseNext: null,
  };
}

function refused(request: WireRequest, code: string, detail: JsonValue): Outcome<JsonValue> {
  return classify(request.contract, null, { status: 200, body: JSON.stringify([{ error: { code, detail } }]) });
}

const completed = (value: JsonValue): Outcome<JsonValue> => ({ status: "completed", value });

/** Writes the rows that `project-admin` of `project` materializes. */
function materialize(state: ControlState, project: string, principal: string): void {
  state.projectAdmins.add(`${project} ${principal}`);
  for (const env of PROJECTS[project] ?? []) {
    state.memberships.add(`${project} ${env} ${principal}`);
  }
}

/** The transport over `state`, and every request it took. */
export function controlStub(state: ControlState = controlState()): { transport: Transport; sent: WireRequest[] } {
  const sent: WireRequest[] = [];
  const answer = (request: WireRequest): Outcome<JsonValue> => {
    const item = (request.items[0] ?? {}) as { [key: string]: JsonValue };
    const value = (item["value"] ?? {}) as { [key: string]: JsonValue };
    const route = request.operation.slice(0, request.operation.indexOf("@"));
    const principal = String(value["principal_id"] ?? "");
    const project = String(value["project"] ?? item["project"] ?? "");
    if (request.method === "POST" && state.refuseNext !== null) {
      const { code, detail } = state.refuseNext;
      state.refuseNext = null;
      return refused(request, code, detail);
    }
    switch (route) {
      case "wamn-control:user/list":
        return completed({
          users: [...state.members.entries()]
            .sort(([, a], [, b]) => a.email.localeCompare(b.email))
            .map(([id, member]) => ({
              principal_id: id,
              email: member.email,
              display_name: member.displayName,
              status: member.active ? "active" : "inactive",
              org_admin: state.orgAdmins.has(id),
            })),
        });
      case "wamn-control:project/list":
        return completed({ projects: Object.keys(PROJECTS) });
      case "wamn-control:environment/list":
        return completed({ environments: [...(PROJECTS[project] ?? [])] });
      case "wamn-control:user/invite": {
        state.members.set(NEW, {
          email: String(value["email"]),
          displayName: String(value["display_name"]),
          active: true,
        });
        if (value["org_admin"] === true) {
          state.orgAdmins.add(NEW);
        }
        for (const name of (value["project_admins"] ?? []) as string[]) {
          materialize(state, name, NEW);
        }
        for (const row of (value["memberships"] ?? []) as { project: string; env: string }[]) {
          state.memberships.add(`${row.project} ${row.env} ${NEW}`);
        }
        return completed({ principal_id: NEW, enrolled: false, invited: true });
      }
      case "wamn-control:user/activate":
      case "wamn-control:user/deactivate": {
        const member = state.members.get(principal);
        if (member === undefined) {
          return refused(request, "user_not_found", { field: "principal_id" });
        }
        member.active = route.endsWith("/activate");
        return completed({ principal_id: principal, status: member.active ? "active" : "inactive" });
      }
      case "wamn-control:org-admin/grant":
        if (state.members.get(principal)?.active !== true) {
          return refused(request, "user_not_active", { field: "principal_id" });
        }
        state.orgAdmins.add(principal);
        for (const name of Object.keys(PROJECTS)) {
          materialize(state, name, principal);
        }
        return completed({ principal_id: principal, org_admin: true });
      case "wamn-control:org-admin/revoke":
        state.orgAdmins.delete(principal);
        for (const name of Object.keys(PROJECTS)) {
          state.projectAdmins.delete(`${name} ${principal}`);
        }
        return completed({ principal_id: principal, org_admin: false });
      case "wamn-control:member/list":
        return completed({
          members: [...state.members.entries()]
            .filter(
              ([id]) =>
                state.projectAdmins.has(`${project} ${id}`) ||
                (PROJECTS[project] ?? []).some((env) => state.memberships.has(`${project} ${env} ${id}`)),
            )
            .sort(([, a], [, b]) => a.email.localeCompare(b.email))
            .map(([id, member]) => ({
              principal_id: id,
              email: member.email,
              display_name: member.displayName,
              org_admin: state.orgAdmins.has(id),
              project_admin: state.projectAdmins.has(`${project} ${id}`),
              environments: (PROJECTS[project] ?? []).filter((env) => state.memberships.has(`${project} ${env} ${id}`)),
            })),
        });
      case "wamn-control:member/grant":
        state.memberships.add(`${project} ${String(value["env"])} ${principal}`);
        return completed({ project, env: String(value["env"]), principal_id: principal, member: true });
      case "wamn-control:member/revoke":
        if (state.orgAdmins.has(principal) || state.projectAdmins.has(`${project} ${principal}`)) {
          return refused(request, "admin_covered", { field: "principal_id" });
        }
        state.memberships.delete(`${project} ${String(value["env"])} ${principal}`);
        return completed({ project, env: String(value["env"]), principal_id: principal, member: false });
      case "wamn-control:project-admin/grant":
        materialize(state, project, principal);
        return completed({ project, principal_id: principal, project_admin: true });
      case "wamn-control:project-admin/revoke":
        if (state.orgAdmins.has(principal)) {
          return refused(request, "admin_covered", { field: "principal_id" });
        }
        state.projectAdmins.delete(`${project} ${principal}`);
        return completed({ project, principal_id: principal, project_admin: false });
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
