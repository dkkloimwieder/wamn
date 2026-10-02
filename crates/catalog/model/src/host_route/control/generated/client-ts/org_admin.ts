// @generated from the client-contract IR; do not edit.
//
// `org_admin` operations of package `wamn_control`.

import type { FieldMap, OperationRoute, Outcome, Transport, Uuid } from "@wamn/web-runtime";
import { reviveOutcome, toWire } from "@wamn/web-runtime";

/** Input for `wamn-control:org-admin/grant@0.1.0`. */
export interface OrgAdminGrantRequest {
  /** `text` */
  requestId: string;
  /** `object` */
  value: OrgAdminGrantRequestValue;
}

export interface OrgAdminGrantRequestValue {
  /** `uuid` */
  principalId: Uuid;
}

/** What `wamn-control:org-admin/grant@0.1.0` calls its input members. */
export const ORG_ADMIN_GRANT_REQUEST_FIELDS: FieldMap = {
  "request_id": "requestId",
  "value": {
    member: "value",
    fields: {
      "principal_id": "principalId",
    },
  },
};

/** Result of `wamn-control:org-admin/grant@0.1.0`. */
export interface OrgAdminGrantResult {
  /** `boolean` */
  readonly orgAdmin: boolean;
  /** `uuid` */
  readonly principalId: Uuid;
}

/** What `wamn-control:org-admin/grant@0.1.0` calls its result members. */
export const ORG_ADMIN_GRANT_RESULT_FIELDS: FieldMap = {
  "org_admin": "orgAdmin",
  "principal_id": "principalId",
};

/**
 * Where the release publishes `wamn-control:org-admin/grant@0.1.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const ORG_ADMIN_GRANT_ROUTE: OperationRoute = {
  operation: "wamn-control:org-admin/grant@0.1.0",
  method: "POST",
  template: "/wamn_control/org_admin/grant",
  freshOnly: false,
  contract: {
    resultClass: "one",
    partialSchema: null,
    errors: [
      { literal: "application_write_incomplete", required: ["environment"], sources: ["transaction_invariant"], text: "The application rows stopped at this environment. The completed environments are done." },
      { literal: "invalid_input", required: ["field"], sources: ["malformed_input"], text: null },
      { literal: "permission_denied", required: ["operation"], sources: ["permission_denied"], text: null },
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

/** Invoke `wamn-control:org-admin/grant@0.1.0` through a transport the application supplies. */
export async function grant(
  transport: Transport,
  items: readonly OrgAdminGrantRequest[],
): Promise<Outcome<OrgAdminGrantResult>> {
  return reviveOutcome<OrgAdminGrantResult>(
    await transport.invoke({
      ...ORG_ADMIN_GRANT_ROUTE,
      items: items.map((item) => toWire(item, ORG_ADMIN_GRANT_REQUEST_FIELDS)),
    }),
    ORG_ADMIN_GRANT_RESULT_FIELDS,
  );
}

/** Input for `wamn-control:org-admin/revoke@0.1.0`. */
export interface OrgAdminRevokeRequest {
  /** `text` */
  requestId: string;
  /** `object` */
  value: OrgAdminRevokeRequestValue;
}

export interface OrgAdminRevokeRequestValue {
  /** `uuid` */
  principalId: Uuid;
}

/** What `wamn-control:org-admin/revoke@0.1.0` calls its input members. */
export const ORG_ADMIN_REVOKE_REQUEST_FIELDS: FieldMap = {
  "request_id": "requestId",
  "value": {
    member: "value",
    fields: {
      "principal_id": "principalId",
    },
  },
};

/** Result of `wamn-control:org-admin/revoke@0.1.0`. */
export interface OrgAdminRevokeResult {
  /** `boolean` */
  readonly orgAdmin: boolean;
  /** `uuid` */
  readonly principalId: Uuid;
}

/** What `wamn-control:org-admin/revoke@0.1.0` calls its result members. */
export const ORG_ADMIN_REVOKE_RESULT_FIELDS: FieldMap = {
  "org_admin": "orgAdmin",
  "principal_id": "principalId",
};

/**
 * Where the release publishes `wamn-control:org-admin/revoke@0.1.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const ORG_ADMIN_REVOKE_ROUTE: OperationRoute = {
  operation: "wamn-control:org-admin/revoke@0.1.0",
  method: "POST",
  template: "/wamn_control/org_admin/revoke",
  freshOnly: false,
  contract: {
    resultClass: "one",
    partialSchema: null,
    errors: [
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

/** Invoke `wamn-control:org-admin/revoke@0.1.0` through a transport the application supplies. */
export async function revoke(
  transport: Transport,
  items: readonly OrgAdminRevokeRequest[],
): Promise<Outcome<OrgAdminRevokeResult>> {
  return reviveOutcome<OrgAdminRevokeResult>(
    await transport.invoke({
      ...ORG_ADMIN_REVOKE_ROUTE,
      items: items.map((item) => toWire(item, ORG_ADMIN_REVOKE_REQUEST_FIELDS)),
    }),
    ORG_ADMIN_REVOKE_RESULT_FIELDS,
  );
}
