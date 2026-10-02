// @generated from the client-contract IR; do not edit.
//
// `permission` operations of package `wamn_control`.

import type { FieldMap, OperationRoute, Outcome, Transport } from "@wamn/web-runtime";
import { reviveOutcome, toWire } from "@wamn/web-runtime";

/** Input for `wamn-control:permission/grant@0.3.0`. */
export interface PermissionGrantRequest {
  /** `text` */
  requestId: string;
  /** `object` */
  value: PermissionGrantRequestValue;
}

export interface PermissionGrantRequestValue {
  /** `text` */
  operation: string;
  /** `text` */
  role: string;
}

/** What `wamn-control:permission/grant@0.3.0` calls its input members. */
export const PERMISSION_GRANT_REQUEST_FIELDS: FieldMap = {
  "request_id": "requestId",
  "value": {
    member: "value",
    fields: {
      "operation": "operation",
      "role": "role",
    },
  },
};

/** Result of `wamn-control:permission/grant@0.3.0`. */
export interface PermissionGrantResult {
  /** `array` */
  readonly closure: readonly string[];
  /** `int32` */
  readonly rowsAdded: number;
}

/** What `wamn-control:permission/grant@0.3.0` calls its result members. */
export const PERMISSION_GRANT_RESULT_FIELDS: FieldMap = {
  "closure": "closure",
  "rows_added": "rowsAdded",
};

/**
 * Where the release publishes `wamn-control:permission/grant@0.3.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const PERMISSION_GRANT_ROUTE: OperationRoute = {
  operation: "wamn-control:permission/grant@0.3.0",
  method: "POST",
  template: "/wamn_control/permission/grant",
  freshOnly: false,
  contract: {
    resultClass: "one",
    partialSchema: null,
    errors: [
      { literal: "admin_fixed", required: ["field"], sources: ["transaction_invariant"], text: "The admin role is built in. It holds every operation, and it cannot be created, deleted or given a permission." },
      { literal: "invalid_input", required: ["field"], sources: ["malformed_input"], text: null },
      { literal: "operation_not_grantable", required: ["field"], sources: ["transaction_invariant"], text: "This operation is fixed to admin or to every member, so no role takes it." },
      { literal: "operation_not_served", required: ["field"], sources: ["transaction_invariant"], text: "The current release does not serve this operation." },
      { literal: "permission_denied", required: ["operation"], sources: ["permission_denied"], text: null },
      { literal: "release_not_current", required: ["field"], sources: ["transaction_invariant"], text: "The release of this host is no longer the current release. Try again when the host serves the current release." },
      { literal: "role_not_found", required: ["field"], sources: ["transaction_invariant"], text: "This role does not exist." },
    ],
    replay: null,
    direct: true,
    type: "command",
    transaction: "explicit_per_input",
    reads: [],
    writes: null,
  },
};

/** Invoke `wamn-control:permission/grant@0.3.0` through a transport the application supplies. */
export async function grant(
  transport: Transport,
  items: readonly PermissionGrantRequest[],
): Promise<Outcome<PermissionGrantResult>> {
  return reviveOutcome<PermissionGrantResult>(
    await transport.invoke({
      ...PERMISSION_GRANT_ROUTE,
      items: items.map((item) => toWire(item, PERMISSION_GRANT_REQUEST_FIELDS)),
    }),
    PERMISSION_GRANT_RESULT_FIELDS,
  );
}

/** Input for `wamn-control:permission/list@0.3.0`. */
export interface PermissionListRequest {
  /** `text` */
  role: string;
}

/** What `wamn-control:permission/list@0.3.0` calls its input members. */
export const PERMISSION_LIST_REQUEST_FIELDS: FieldMap = {
  "role": "role",
};

/** Result of `wamn-control:permission/list@0.3.0`. */
export interface PermissionListResult {
  /** `boolean` */
  readonly admin: boolean;
  /** `array` */
  readonly operations: readonly PermissionListResultOperations[];
  /** `text` */
  readonly role: string;
}

export interface PermissionListResultOperations {
  /** `boolean` */
  readonly adminOnly: boolean;
  /** `boolean` */
  readonly grantable: boolean;
  /** `text` */
  readonly operation: string;
  /** `array` */
  readonly requiredBy: readonly string[];
  /** `boolean` */
  readonly selected: boolean;
  /** `boolean` */
  readonly served: boolean;
}

/** What `wamn-control:permission/list@0.3.0` calls its result members. */
export const PERMISSION_LIST_RESULT_FIELDS: FieldMap = {
  "admin": "admin",
  "operations": {
    member: "operations",
    fields: {
      "admin_only": "adminOnly",
      "grantable": "grantable",
      "operation": "operation",
      "required_by": "requiredBy",
      "selected": "selected",
      "served": "served",
    },
  },
  "role": "role",
};

/**
 * Where the release publishes `wamn-control:permission/list@0.3.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const PERMISSION_LIST_ROUTE: OperationRoute = {
  operation: "wamn-control:permission/list@0.3.0",
  method: "GET",
  template: "/wamn_control/permission/list",
  freshOnly: false,
  contract: {
    resultClass: "one",
    partialSchema: null,
    errors: [
      { literal: "invalid_input", required: ["field"], sources: ["malformed_input"], text: null },
      { literal: "permission_denied", required: ["operation"], sources: ["permission_denied"], text: null },
      { literal: "role_not_found", required: ["field"], sources: ["transaction_invariant"], text: "This role does not exist." },
    ],
    replay: null,
    direct: true,
    type: "get",
    transaction: "implicit",
    reads: [],
    writes: null,
  },
};

/** Invoke `wamn-control:permission/list@0.3.0` through a transport the application supplies. */
export async function list(
  transport: Transport,
  items: readonly PermissionListRequest[],
): Promise<Outcome<PermissionListResult>> {
  return reviveOutcome<PermissionListResult>(
    await transport.invoke({
      ...PERMISSION_LIST_ROUTE,
      items: items.map((item) => toWire(item, PERMISSION_LIST_REQUEST_FIELDS)),
    }),
    PERMISSION_LIST_RESULT_FIELDS,
  );
}

/** Input for `wamn-control:permission/mine@0.3.0`. */
export interface PermissionMineRequest {
}

/** What `wamn-control:permission/mine@0.3.0` calls its input members. */
export const PERMISSION_MINE_REQUEST_FIELDS: FieldMap = {};

/** Result of `wamn-control:permission/mine@0.3.0`. */
export interface PermissionMineResult {
  /** `boolean` */
  readonly admin: boolean;
  /** `array` */
  readonly permissions: readonly string[];
}

/** What `wamn-control:permission/mine@0.3.0` calls its result members. */
export const PERMISSION_MINE_RESULT_FIELDS: FieldMap = {
  "admin": "admin",
  "permissions": "permissions",
};

/**
 * Where the release publishes `wamn-control:permission/mine@0.3.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const PERMISSION_MINE_ROUTE: OperationRoute = {
  operation: "wamn-control:permission/mine@0.3.0",
  method: "GET",
  template: "/wamn_control/permission/mine",
  freshOnly: false,
  contract: {
    resultClass: "one",
    partialSchema: null,
    errors: [
      { literal: "permission_denied", required: ["operation"], sources: ["permission_denied"], text: null },
    ],
    replay: null,
    direct: true,
    type: "get",
    transaction: "implicit",
    reads: [],
    writes: null,
  },
};

/** Invoke `wamn-control:permission/mine@0.3.0` through a transport the application supplies. */
export async function mine(
  transport: Transport,
  items: readonly PermissionMineRequest[],
): Promise<Outcome<PermissionMineResult>> {
  return reviveOutcome<PermissionMineResult>(
    await transport.invoke({
      ...PERMISSION_MINE_ROUTE,
      items: items.map((item) => toWire(item, PERMISSION_MINE_REQUEST_FIELDS)),
    }),
    PERMISSION_MINE_RESULT_FIELDS,
  );
}

/** Input for `wamn-control:permission/revoke@0.3.0`. */
export interface PermissionRevokeRequest {
  /** `text` */
  requestId: string;
  /** `object` */
  value: PermissionRevokeRequestValue;
}

export interface PermissionRevokeRequestValue {
  /** `text` */
  operation: string;
  /** `text` */
  role: string;
}

/** What `wamn-control:permission/revoke@0.3.0` calls its input members. */
export const PERMISSION_REVOKE_REQUEST_FIELDS: FieldMap = {
  "request_id": "requestId",
  "value": {
    member: "value",
    fields: {
      "operation": "operation",
      "role": "role",
    },
  },
};

/** Result of `wamn-control:permission/revoke@0.3.0`. */
export interface PermissionRevokeResult {
  /** `array` */
  readonly stillRequiredBy: readonly string[];
}

/** What `wamn-control:permission/revoke@0.3.0` calls its result members. */
export const PERMISSION_REVOKE_RESULT_FIELDS: FieldMap = {
  "still_required_by": "stillRequiredBy",
};

/**
 * Where the release publishes `wamn-control:permission/revoke@0.3.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const PERMISSION_REVOKE_ROUTE: OperationRoute = {
  operation: "wamn-control:permission/revoke@0.3.0",
  method: "POST",
  template: "/wamn_control/permission/revoke",
  freshOnly: false,
  contract: {
    resultClass: "one",
    partialSchema: null,
    errors: [
      { literal: "invalid_input", required: ["field"], sources: ["malformed_input"], text: null },
      { literal: "permission_denied", required: ["operation"], sources: ["permission_denied"], text: null },
      { literal: "permission_not_held", required: ["field"], sources: ["transaction_invariant"], text: "The role does not hold this operation." },
      { literal: "permission_not_selected", required: ["field"], sources: ["transaction_invariant"], text: "The role holds this operation only because another selected operation requires it. Revoke that operation instead." },
      { literal: "role_not_found", required: ["field"], sources: ["transaction_invariant"], text: "This role does not exist." },
    ],
    replay: null,
    direct: true,
    type: "command",
    transaction: "explicit_per_input",
    reads: [],
    writes: null,
  },
};

/** Invoke `wamn-control:permission/revoke@0.3.0` through a transport the application supplies. */
export async function revoke(
  transport: Transport,
  items: readonly PermissionRevokeRequest[],
): Promise<Outcome<PermissionRevokeResult>> {
  return reviveOutcome<PermissionRevokeResult>(
    await transport.invoke({
      ...PERMISSION_REVOKE_ROUTE,
      items: items.map((item) => toWire(item, PERMISSION_REVOKE_REQUEST_FIELDS)),
    }),
    PERMISSION_REVOKE_RESULT_FIELDS,
  );
}
