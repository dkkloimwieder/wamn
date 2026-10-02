// @generated from the client-contract IR; do not edit.
//
// `user_role` operations of package `wamn_control`.

import type { FieldMap, OperationRoute, Outcome, Transport, Uuid } from "@wamn/web-runtime";
import { reviveOutcome, toWire } from "@wamn/web-runtime";

/** Input for `wamn-control:user-role/grant@0.1.0`. */
export interface UserRoleGrantRequest {
  /** `text` */
  requestId: string;
  /** `object` */
  value: UserRoleGrantRequestValue;
}

export interface UserRoleGrantRequestValue {
  /** `text` */
  role: string;
  /** `uuid` */
  userId: Uuid;
}

/** What `wamn-control:user-role/grant@0.1.0` calls its input members. */
export const USER_ROLE_GRANT_REQUEST_FIELDS: FieldMap = {
  "request_id": "requestId",
  "value": {
    member: "value",
    fields: {
      "role": "role",
      "user_id": "userId",
    },
  },
};

/** Result of `wamn-control:user-role/grant@0.1.0`. */
export interface UserRoleGrantResult {
  /** `boolean` */
  readonly granted: boolean;
}

/** What `wamn-control:user-role/grant@0.1.0` calls its result members. */
export const USER_ROLE_GRANT_RESULT_FIELDS: FieldMap = {
  "granted": "granted",
};

/**
 * Where the release publishes `wamn-control:user-role/grant@0.1.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const USER_ROLE_GRANT_ROUTE: OperationRoute = {
  operation: "wamn-control:user-role/grant@0.1.0",
  method: "POST",
  template: "/wamn_control/user_role/grant",
  freshOnly: false,
  contract: {
    resultClass: "one",
    partialSchema: null,
    errors: [
      { literal: "invalid_input", required: ["field"], sources: ["malformed_input"], text: null },
      { literal: "permission_denied", required: ["operation"], sources: ["permission_denied"], text: null },
      { literal: "role_not_found", required: ["field"], sources: ["transaction_invariant"], text: "This role does not exist." },
      { literal: "user_not_found", required: ["field"], sources: ["transaction_invariant"], text: "The application has no such user." },
    ],
    replay: null,
    direct: true,
    type: "command",
    transaction: "explicit_per_input",
    reads: [],
    writes: null,
  },
};

/** Invoke `wamn-control:user-role/grant@0.1.0` through a transport the application supplies. */
export async function grant(
  transport: Transport,
  items: readonly UserRoleGrantRequest[],
): Promise<Outcome<UserRoleGrantResult>> {
  return reviveOutcome<UserRoleGrantResult>(
    await transport.invoke({
      ...USER_ROLE_GRANT_ROUTE,
      items: items.map((item) => toWire(item, USER_ROLE_GRANT_REQUEST_FIELDS)),
    }),
    USER_ROLE_GRANT_RESULT_FIELDS,
  );
}

/** Input for `wamn-control:user-role/revoke@0.1.0`. */
export interface UserRoleRevokeRequest {
  /** `text` */
  requestId: string;
  /** `object` */
  value: UserRoleRevokeRequestValue;
}

export interface UserRoleRevokeRequestValue {
  /** `text` */
  role: string;
  /** `uuid` */
  userId: Uuid;
}

/** What `wamn-control:user-role/revoke@0.1.0` calls its input members. */
export const USER_ROLE_REVOKE_REQUEST_FIELDS: FieldMap = {
  "request_id": "requestId",
  "value": {
    member: "value",
    fields: {
      "role": "role",
      "user_id": "userId",
    },
  },
};

/** Result of `wamn-control:user-role/revoke@0.1.0`. */
export interface UserRoleRevokeResult {
  /** `boolean` */
  readonly revoked: boolean;
}

/** What `wamn-control:user-role/revoke@0.1.0` calls its result members. */
export const USER_ROLE_REVOKE_RESULT_FIELDS: FieldMap = {
  "revoked": "revoked",
};

/**
 * Where the release publishes `wamn-control:user-role/revoke@0.1.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const USER_ROLE_REVOKE_ROUTE: OperationRoute = {
  operation: "wamn-control:user-role/revoke@0.1.0",
  method: "POST",
  template: "/wamn_control/user_role/revoke",
  freshOnly: false,
  contract: {
    resultClass: "one",
    partialSchema: null,
    errors: [
      { literal: "admin_covered", required: ["field"], sources: ["transaction_invariant"], text: "The user holds project-admin or org-admin, so admin stays. Revoke that first." },
      { literal: "invalid_input", required: ["field"], sources: ["malformed_input"], text: null },
      { literal: "permission_denied", required: ["operation"], sources: ["permission_denied"], text: null },
      { literal: "user_not_found", required: ["field"], sources: ["transaction_invariant"], text: "The application has no such user." },
    ],
    replay: null,
    direct: true,
    type: "command",
    transaction: "explicit_per_input",
    reads: [],
    writes: null,
  },
};

/** Invoke `wamn-control:user-role/revoke@0.1.0` through a transport the application supplies. */
export async function revoke(
  transport: Transport,
  items: readonly UserRoleRevokeRequest[],
): Promise<Outcome<UserRoleRevokeResult>> {
  return reviveOutcome<UserRoleRevokeResult>(
    await transport.invoke({
      ...USER_ROLE_REVOKE_ROUTE,
      items: items.map((item) => toWire(item, USER_ROLE_REVOKE_REQUEST_FIELDS)),
    }),
    USER_ROLE_REVOKE_RESULT_FIELDS,
  );
}
