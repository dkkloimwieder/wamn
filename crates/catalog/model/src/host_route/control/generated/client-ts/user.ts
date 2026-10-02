// @generated from the client-contract IR; do not edit.
//
// `user` operations of package `wamn_control`.

import type { FieldMap, OperationRoute, Outcome, Transport, Uuid } from "@wamn/web-runtime";
import { reviveOutcome, toWire } from "@wamn/web-runtime";

/** Input for `wamn-control:user/activate@0.3.0`. */
export interface UserActivateRequest {
  /** `text` */
  requestId: string;
  /** `object` */
  value: UserActivateRequestValue;
}

export interface UserActivateRequestValue {
  /** `uuid` */
  principalId: Uuid;
}

/** What `wamn-control:user/activate@0.3.0` calls its input members. */
export const USER_ACTIVATE_REQUEST_FIELDS: FieldMap = {
  "request_id": "requestId",
  "value": {
    member: "value",
    fields: {
      "principal_id": "principalId",
    },
  },
};

/** Result of `wamn-control:user/activate@0.3.0`. */
export interface UserActivateResult {
  /** `uuid` */
  readonly principalId: Uuid;
  /** `text` */
  readonly status: string;
}

/** What `wamn-control:user/activate@0.3.0` calls its result members. */
export const USER_ACTIVATE_RESULT_FIELDS: FieldMap = {
  "principal_id": "principalId",
  "status": "status",
};

/**
 * Where the release publishes `wamn-control:user/activate@0.3.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const USER_ACTIVATE_ROUTE: OperationRoute = {
  operation: "wamn-control:user/activate@0.3.0",
  method: "POST",
  template: "/wamn_control/user/activate",
  freshOnly: false,
  contract: {
    resultClass: "one",
    partialSchema: null,
    errors: [
      { literal: "invalid_input", required: ["field"], sources: ["malformed_input"], text: null },
      { literal: "permission_denied", required: ["operation"], sources: ["permission_denied"], text: null },
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

/** Invoke `wamn-control:user/activate@0.3.0` through a transport the application supplies. */
export async function activate(
  transport: Transport,
  items: readonly UserActivateRequest[],
): Promise<Outcome<UserActivateResult>> {
  return reviveOutcome<UserActivateResult>(
    await transport.invoke({
      ...USER_ACTIVATE_ROUTE,
      items: items.map((item) => toWire(item, USER_ACTIVATE_REQUEST_FIELDS)),
    }),
    USER_ACTIVATE_RESULT_FIELDS,
  );
}

/** Input for `wamn-control:user/deactivate@0.3.0`. */
export interface UserDeactivateRequest {
  /** `text` */
  requestId: string;
  /** `object` */
  value: UserDeactivateRequestValue;
}

export interface UserDeactivateRequestValue {
  /** `uuid` */
  principalId: Uuid;
}

/** What `wamn-control:user/deactivate@0.3.0` calls its input members. */
export const USER_DEACTIVATE_REQUEST_FIELDS: FieldMap = {
  "request_id": "requestId",
  "value": {
    member: "value",
    fields: {
      "principal_id": "principalId",
    },
  },
};

/** Result of `wamn-control:user/deactivate@0.3.0`. */
export interface UserDeactivateResult {
  /** `uuid` */
  readonly principalId: Uuid;
  /** `text` */
  readonly status: string;
}

/** What `wamn-control:user/deactivate@0.3.0` calls its result members. */
export const USER_DEACTIVATE_RESULT_FIELDS: FieldMap = {
  "principal_id": "principalId",
  "status": "status",
};

/**
 * Where the release publishes `wamn-control:user/deactivate@0.3.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const USER_DEACTIVATE_ROUTE: OperationRoute = {
  operation: "wamn-control:user/deactivate@0.3.0",
  method: "POST",
  template: "/wamn_control/user/deactivate",
  freshOnly: false,
  contract: {
    resultClass: "one",
    partialSchema: null,
    errors: [
      { literal: "application_write_incomplete", required: ["environment"], sources: ["transaction_invariant"], text: "The application rows stopped at this environment. The completed environments are done." },
      { literal: "invalid_input", required: ["field"], sources: ["malformed_input"], text: null },
      { literal: "permission_denied", required: ["operation"], sources: ["permission_denied"], text: null },
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

/** Invoke `wamn-control:user/deactivate@0.3.0` through a transport the application supplies. */
export async function deactivate(
  transport: Transport,
  items: readonly UserDeactivateRequest[],
): Promise<Outcome<UserDeactivateResult>> {
  return reviveOutcome<UserDeactivateResult>(
    await transport.invoke({
      ...USER_DEACTIVATE_ROUTE,
      items: items.map((item) => toWire(item, USER_DEACTIVATE_REQUEST_FIELDS)),
    }),
    USER_DEACTIVATE_RESULT_FIELDS,
  );
}

/** Input for `wamn-control:user/invite@0.3.0`. */
export interface UserInviteRequest {
  /** `text` */
  requestId: string;
  /** `object` */
  value: UserInviteRequestValue;
}

export interface UserInviteRequestValue {
  /** `text` */
  displayName: string;
  /** `text` */
  email: string;
  /** `array` */
  memberships: UserInviteRequestValueMemberships[];
  /** `boolean` */
  orgAdmin: boolean | null;
  /** `array` */
  projectAdmins: string[];
}

export interface UserInviteRequestValueMemberships {
  /** `text` */
  env: string;
  /** `text` */
  project: string;
}

/** What `wamn-control:user/invite@0.3.0` calls its input members. */
export const USER_INVITE_REQUEST_FIELDS: FieldMap = {
  "request_id": "requestId",
  "value": {
    member: "value",
    fields: {
      "display_name": "displayName",
      "email": "email",
      "memberships": {
        member: "memberships",
        fields: {
          "env": "env",
          "project": "project",
        },
      },
      "org_admin": "orgAdmin",
      "project_admins": "projectAdmins",
    },
  },
};

/** Result of `wamn-control:user/invite@0.3.0`. */
export interface UserInviteResult {
  /** `boolean` */
  readonly enrolled: boolean;
  /** `boolean` */
  readonly invited: boolean;
  /** `uuid` */
  readonly principalId: Uuid;
}

/** What `wamn-control:user/invite@0.3.0` calls its result members. */
export const USER_INVITE_RESULT_FIELDS: FieldMap = {
  "enrolled": "enrolled",
  "invited": "invited",
  "principal_id": "principalId",
};

/**
 * Where the release publishes `wamn-control:user/invite@0.3.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const USER_INVITE_ROUTE: OperationRoute = {
  operation: "wamn-control:user/invite@0.3.0",
  method: "POST",
  template: "/wamn_control/user/invite",
  freshOnly: false,
  contract: {
    resultClass: "one",
    partialSchema: null,
    errors: [
      { literal: "application_write_incomplete", required: ["environment"], sources: ["transaction_invariant"], text: "The application rows stopped at this environment. The completed environments are done." },
      { literal: "environment_not_found", required: ["field"], sources: ["transaction_invariant"], text: "This environment is not an environment of the org." },
      { literal: "invalid_input", required: ["field"], sources: ["malformed_input"], text: null },
      { literal: "permission_denied", required: ["operation"], sources: ["permission_denied"], text: null },
      { literal: "project_not_found", required: ["field"], sources: ["transaction_invariant"], text: "This project is not a project of the org." },
      { literal: "user_not_found", required: ["field"], sources: ["transaction_invariant"], text: "The org has no such user." },
      { literal: "user_refused", required: ["field"], sources: ["transaction_invariant"], text: "Identity refused the user of this email, for example a disabled user." },
    ],
    replay: null,
    direct: true,
    type: "command",
    transaction: "explicit_per_input",
    reads: [],
    writes: null,
  },
};

/** Invoke `wamn-control:user/invite@0.3.0` through a transport the application supplies. */
export async function invite(
  transport: Transport,
  items: readonly UserInviteRequest[],
): Promise<Outcome<UserInviteResult>> {
  return reviveOutcome<UserInviteResult>(
    await transport.invoke({
      ...USER_INVITE_ROUTE,
      items: items.map((item) => toWire(item, USER_INVITE_REQUEST_FIELDS)),
    }),
    USER_INVITE_RESULT_FIELDS,
  );
}

/** Input for `wamn-control:user/list@0.3.0`. */
export interface UserListRequest {
}

/** What `wamn-control:user/list@0.3.0` calls its input members. */
export const USER_LIST_REQUEST_FIELDS: FieldMap = {};

/** Result of `wamn-control:user/list@0.3.0`. */
export interface UserListResult {
  /** `array` */
  readonly users: readonly UserListResultUsers[];
}

export interface UserListResultUsers {
  /** `text` */
  readonly displayName: string;
  /** `text` */
  readonly email: string;
  /** `boolean` */
  readonly orgAdmin: boolean;
  /** `uuid` */
  readonly principalId: Uuid;
  /** `text` */
  readonly status: string;
}

/** What `wamn-control:user/list@0.3.0` calls its result members. */
export const USER_LIST_RESULT_FIELDS: FieldMap = {
  "users": {
    member: "users",
    fields: {
      "display_name": "displayName",
      "email": "email",
      "org_admin": "orgAdmin",
      "principal_id": "principalId",
      "status": "status",
    },
  },
};

/**
 * Where the release publishes `wamn-control:user/list@0.3.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const USER_LIST_ROUTE: OperationRoute = {
  operation: "wamn-control:user/list@0.3.0",
  method: "GET",
  template: "/wamn_control/user/list",
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

/** Invoke `wamn-control:user/list@0.3.0` through a transport the application supplies. */
export async function list(
  transport: Transport,
  items: readonly UserListRequest[],
): Promise<Outcome<UserListResult>> {
  return reviveOutcome<UserListResult>(
    await transport.invoke({
      ...USER_LIST_ROUTE,
      items: items.map((item) => toWire(item, USER_LIST_REQUEST_FIELDS)),
    }),
    USER_LIST_RESULT_FIELDS,
  );
}
