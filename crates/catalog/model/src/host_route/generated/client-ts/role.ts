// @generated from the client-contract IR; do not edit.
//
// `role` operations of package `wamn_control`.

import type { FieldMap, OperationRoute, Outcome, Transport } from "@wamn/web-runtime";
import { reviveOutcome, toWire } from "@wamn/web-runtime";

/** Input for `wamn-control:role/create@0.2.0`. */
export interface RoleCreateRequest {
  /** `text` */
  requestId: string;
  /** `object` */
  value: RoleCreateRequestValue;
}

export interface RoleCreateRequestValue {
  /** `text` */
  role: string;
}

/** What `wamn-control:role/create@0.2.0` calls its input members. */
export const ROLE_CREATE_REQUEST_FIELDS: FieldMap = {
  "request_id": "requestId",
  "value": {
    member: "value",
    fields: {
      "role": "role",
    },
  },
};

/** Result of `wamn-control:role/create@0.2.0`. */
export interface RoleCreateResult {
  /** `boolean` */
  readonly created: boolean;
}

/** What `wamn-control:role/create@0.2.0` calls its result members. */
export const ROLE_CREATE_RESULT_FIELDS: FieldMap = {
  "created": "created",
};

/**
 * Where the release publishes `wamn-control:role/create@0.2.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const ROLE_CREATE_ROUTE: OperationRoute = {
  operation: "wamn-control:role/create@0.2.0",
  method: "POST",
  template: "/wamn_control/role/create",
  freshOnly: false,
  contract: {
    resultClass: "one",
    partialSchema: null,
    errors: [
      { literal: "admin_fixed", required: ["field"], sources: ["transaction_invariant"], text: "The admin role is built in. It holds every operation, and it cannot be created, deleted or given a permission." },
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

/** Invoke `wamn-control:role/create@0.2.0` through a transport the application supplies. */
export async function create(
  transport: Transport,
  items: readonly RoleCreateRequest[],
): Promise<Outcome<RoleCreateResult>> {
  return reviveOutcome<RoleCreateResult>(
    await transport.invoke({
      ...ROLE_CREATE_ROUTE,
      items: items.map((item) => toWire(item, ROLE_CREATE_REQUEST_FIELDS)),
    }),
    ROLE_CREATE_RESULT_FIELDS,
  );
}

/** Input for `wamn-control:role/delete@0.2.0`. */
export interface RoleDeleteRequest {
  /** `text` */
  requestId: string;
  /** `object` */
  value: RoleDeleteRequestValue;
}

export interface RoleDeleteRequestValue {
  /** `text` */
  role: string;
}

/** What `wamn-control:role/delete@0.2.0` calls its input members. */
export const ROLE_DELETE_REQUEST_FIELDS: FieldMap = {
  "request_id": "requestId",
  "value": {
    member: "value",
    fields: {
      "role": "role",
    },
  },
};

/** Result of `wamn-control:role/delete@0.2.0`. */
export interface RoleDeleteResult {
  /** `boolean` */
  readonly deleted: boolean;
}

/** What `wamn-control:role/delete@0.2.0` calls its result members. */
export const ROLE_DELETE_RESULT_FIELDS: FieldMap = {
  "deleted": "deleted",
};

/**
 * Where the release publishes `wamn-control:role/delete@0.2.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const ROLE_DELETE_ROUTE: OperationRoute = {
  operation: "wamn-control:role/delete@0.2.0",
  method: "POST",
  template: "/wamn_control/role/delete",
  freshOnly: false,
  contract: {
    resultClass: "one",
    partialSchema: null,
    errors: [
      { literal: "admin_fixed", required: ["field"], sources: ["transaction_invariant"], text: "The admin role is built in. It holds every operation, and it cannot be created, deleted or given a permission." },
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

/** Invoke `wamn-control:role/delete@0.2.0` through a transport the application supplies. */
export async function delete_(
  transport: Transport,
  items: readonly RoleDeleteRequest[],
): Promise<Outcome<RoleDeleteResult>> {
  return reviveOutcome<RoleDeleteResult>(
    await transport.invoke({
      ...ROLE_DELETE_ROUTE,
      items: items.map((item) => toWire(item, ROLE_DELETE_REQUEST_FIELDS)),
    }),
    ROLE_DELETE_RESULT_FIELDS,
  );
}

/** Input for `wamn-control:role/list@0.2.0`. */
export interface RoleListRequest {
}

/** What `wamn-control:role/list@0.2.0` calls its input members. */
export const ROLE_LIST_REQUEST_FIELDS: FieldMap = {};

/** Result of `wamn-control:role/list@0.2.0`. */
export interface RoleListResult {
  /** `array` */
  readonly roles: readonly string[];
}

/** What `wamn-control:role/list@0.2.0` calls its result members. */
export const ROLE_LIST_RESULT_FIELDS: FieldMap = {
  "roles": "roles",
};

/**
 * Where the release publishes `wamn-control:role/list@0.2.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const ROLE_LIST_ROUTE: OperationRoute = {
  operation: "wamn-control:role/list@0.2.0",
  method: "GET",
  template: "/wamn_control/role/list",
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

/** Invoke `wamn-control:role/list@0.2.0` through a transport the application supplies. */
export async function list(
  transport: Transport,
  items: readonly RoleListRequest[],
): Promise<Outcome<RoleListResult>> {
  return reviveOutcome<RoleListResult>(
    await transport.invoke({
      ...ROLE_LIST_ROUTE,
      items: items.map((item) => toWire(item, ROLE_LIST_REQUEST_FIELDS)),
    }),
    ROLE_LIST_RESULT_FIELDS,
  );
}
