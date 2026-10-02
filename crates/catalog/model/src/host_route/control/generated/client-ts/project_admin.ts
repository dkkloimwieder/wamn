// @generated from the client-contract IR; do not edit.
//
// `project_admin` operations of package `wamn_control`.

import type { FieldMap, OperationRoute, Outcome, Transport, Uuid } from "@wamn/web-runtime";
import { reviveOutcome, toWire } from "@wamn/web-runtime";

/** Input for `wamn-control:project-admin/grant@0.3.0`. */
export interface ProjectAdminGrantRequest {
  /** `text` */
  requestId: string;
  /** `object` */
  value: ProjectAdminGrantRequestValue;
}

export interface ProjectAdminGrantRequestValue {
  /** `uuid` */
  principalId: Uuid;
  /** `text` */
  project: string;
}

/** What `wamn-control:project-admin/grant@0.3.0` calls its input members. */
export const PROJECT_ADMIN_GRANT_REQUEST_FIELDS: FieldMap = {
  "request_id": "requestId",
  "value": {
    member: "value",
    fields: {
      "principal_id": "principalId",
      "project": "project",
    },
  },
};

/** Result of `wamn-control:project-admin/grant@0.3.0`. */
export interface ProjectAdminGrantResult {
  /** `uuid` */
  readonly principalId: Uuid;
  /** `text` */
  readonly project: string;
  /** `boolean` */
  readonly projectAdmin: boolean;
}

/** What `wamn-control:project-admin/grant@0.3.0` calls its result members. */
export const PROJECT_ADMIN_GRANT_RESULT_FIELDS: FieldMap = {
  "principal_id": "principalId",
  "project": "project",
  "project_admin": "projectAdmin",
};

/**
 * Where the release publishes `wamn-control:project-admin/grant@0.3.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const PROJECT_ADMIN_GRANT_ROUTE: OperationRoute = {
  operation: "wamn-control:project-admin/grant@0.3.0",
  method: "POST",
  template: "/wamn_control/project_admin/grant",
  freshOnly: false,
  contract: {
    resultClass: "one",
    partialSchema: null,
    errors: [
      { literal: "application_write_incomplete", required: ["environment"], sources: ["transaction_invariant"], text: "The application rows stopped at this environment. The completed environments are done." },
      { literal: "invalid_input", required: ["field"], sources: ["malformed_input"], text: null },
      { literal: "permission_denied", required: ["operation"], sources: ["permission_denied"], text: null },
      { literal: "project_not_found", required: ["field"], sources: ["transaction_invariant"], text: "This project is not a project of the org." },
      { literal: "user_not_active", required: ["field"], sources: ["transaction_invariant"], text: "The user is not an active member of the org." },
      { literal: "user_not_found", required: ["field"], sources: ["transaction_invariant"], text: "The org has no such user." },
    ],
    replay: null,
    direct: true,
    type: "command",
    transaction: "explicit_per_input",
    reads: [],
    writes: null,
  },
};

/** Invoke `wamn-control:project-admin/grant@0.3.0` through a transport the application supplies. */
export async function grant(
  transport: Transport,
  items: readonly ProjectAdminGrantRequest[],
): Promise<Outcome<ProjectAdminGrantResult>> {
  return reviveOutcome<ProjectAdminGrantResult>(
    await transport.invoke({
      ...PROJECT_ADMIN_GRANT_ROUTE,
      items: items.map((item) => toWire(item, PROJECT_ADMIN_GRANT_REQUEST_FIELDS)),
    }),
    PROJECT_ADMIN_GRANT_RESULT_FIELDS,
  );
}

/** Input for `wamn-control:project-admin/revoke@0.3.0`. */
export interface ProjectAdminRevokeRequest {
  /** `text` */
  requestId: string;
  /** `object` */
  value: ProjectAdminRevokeRequestValue;
}

export interface ProjectAdminRevokeRequestValue {
  /** `uuid` */
  principalId: Uuid;
  /** `text` */
  project: string;
}

/** What `wamn-control:project-admin/revoke@0.3.0` calls its input members. */
export const PROJECT_ADMIN_REVOKE_REQUEST_FIELDS: FieldMap = {
  "request_id": "requestId",
  "value": {
    member: "value",
    fields: {
      "principal_id": "principalId",
      "project": "project",
    },
  },
};

/** Result of `wamn-control:project-admin/revoke@0.3.0`. */
export interface ProjectAdminRevokeResult {
  /** `uuid` */
  readonly principalId: Uuid;
  /** `text` */
  readonly project: string;
  /** `boolean` */
  readonly projectAdmin: boolean;
}

/** What `wamn-control:project-admin/revoke@0.3.0` calls its result members. */
export const PROJECT_ADMIN_REVOKE_RESULT_FIELDS: FieldMap = {
  "principal_id": "principalId",
  "project": "project",
  "project_admin": "projectAdmin",
};

/**
 * Where the release publishes `wamn-control:project-admin/revoke@0.3.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const PROJECT_ADMIN_REVOKE_ROUTE: OperationRoute = {
  operation: "wamn-control:project-admin/revoke@0.3.0",
  method: "POST",
  template: "/wamn_control/project_admin/revoke",
  freshOnly: false,
  contract: {
    resultClass: "one",
    partialSchema: null,
    errors: [
      { literal: "admin_covered", required: ["field"], sources: ["transaction_invariant"], text: "The user holds org-admin or project-admin, which covers this grant. Revoke that first." },
      { literal: "application_write_incomplete", required: ["environment"], sources: ["transaction_invariant"], text: "The application rows stopped at this environment. The completed environments are done." },
      { literal: "invalid_input", required: ["field"], sources: ["malformed_input"], text: null },
      { literal: "permission_denied", required: ["operation"], sources: ["permission_denied"], text: null },
    ],
    replay: null,
    direct: true,
    type: "command",
    transaction: "explicit_per_input",
    reads: [],
    writes: null,
  },
};

/** Invoke `wamn-control:project-admin/revoke@0.3.0` through a transport the application supplies. */
export async function revoke(
  transport: Transport,
  items: readonly ProjectAdminRevokeRequest[],
): Promise<Outcome<ProjectAdminRevokeResult>> {
  return reviveOutcome<ProjectAdminRevokeResult>(
    await transport.invoke({
      ...PROJECT_ADMIN_REVOKE_ROUTE,
      items: items.map((item) => toWire(item, PROJECT_ADMIN_REVOKE_REQUEST_FIELDS)),
    }),
    PROJECT_ADMIN_REVOKE_RESULT_FIELDS,
  );
}
