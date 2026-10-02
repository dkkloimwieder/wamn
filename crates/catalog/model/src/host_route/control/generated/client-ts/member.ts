// @generated from the client-contract IR; do not edit.
//
// `member` operations of package `wamn_control`.

import type { FieldMap, OperationRoute, Outcome, Transport, Uuid } from "@wamn/web-runtime";
import { reviveOutcome, toWire } from "@wamn/web-runtime";

/** Input for `wamn-control:member/grant@0.3.0`. */
export interface MemberGrantRequest {
  /** `text` */
  requestId: string;
  /** `object` */
  value: MemberGrantRequestValue;
}

export interface MemberGrantRequestValue {
  /** `text` */
  env: string;
  /** `uuid` */
  principalId: Uuid;
  /** `text` */
  project: string;
}

/** What `wamn-control:member/grant@0.3.0` calls its input members. */
export const MEMBER_GRANT_REQUEST_FIELDS: FieldMap = {
  "request_id": "requestId",
  "value": {
    member: "value",
    fields: {
      "env": "env",
      "principal_id": "principalId",
      "project": "project",
    },
  },
};

/** Result of `wamn-control:member/grant@0.3.0`. */
export interface MemberGrantResult {
  /** `text` */
  readonly env: string;
  /** `boolean` */
  readonly member: boolean;
  /** `uuid` */
  readonly principalId: Uuid;
  /** `text` */
  readonly project: string;
}

/** What `wamn-control:member/grant@0.3.0` calls its result members. */
export const MEMBER_GRANT_RESULT_FIELDS: FieldMap = {
  "env": "env",
  "member": "member",
  "principal_id": "principalId",
  "project": "project",
};

/**
 * Where the release publishes `wamn-control:member/grant@0.3.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const MEMBER_GRANT_ROUTE: OperationRoute = {
  operation: "wamn-control:member/grant@0.3.0",
  method: "POST",
  template: "/wamn_control/member/grant",
  freshOnly: false,
  contract: {
    resultClass: "one",
    partialSchema: null,
    errors: [
      { literal: "application_write_incomplete", required: ["environment"], sources: ["transaction_invariant"], text: "The application rows stopped at this environment. The completed environments are done." },
      { literal: "environment_not_found", required: ["field"], sources: ["transaction_invariant"], text: "This environment is not an environment of the org." },
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

/** Invoke `wamn-control:member/grant@0.3.0` through a transport the application supplies. */
export async function grant(
  transport: Transport,
  items: readonly MemberGrantRequest[],
): Promise<Outcome<MemberGrantResult>> {
  return reviveOutcome<MemberGrantResult>(
    await transport.invoke({
      ...MEMBER_GRANT_ROUTE,
      items: items.map((item) => toWire(item, MEMBER_GRANT_REQUEST_FIELDS)),
    }),
    MEMBER_GRANT_RESULT_FIELDS,
  );
}

/** Input for `wamn-control:member/list@0.3.0`. */
export interface MemberListRequest {
  /** `text` */
  project: string;
}

/** What `wamn-control:member/list@0.3.0` calls its input members. */
export const MEMBER_LIST_REQUEST_FIELDS: FieldMap = {
  "project": "project",
};

/** Result of `wamn-control:member/list@0.3.0`. */
export interface MemberListResult {
  /** `array` */
  readonly members: readonly MemberListResultMembers[];
}

export interface MemberListResultMembers {
  /** `text` */
  readonly displayName: string;
  /** `text` */
  readonly email: string;
  /** `array` */
  readonly environments: readonly string[];
  /** `boolean` */
  readonly orgAdmin: boolean;
  /** `uuid` */
  readonly principalId: Uuid;
  /** `boolean` */
  readonly projectAdmin: boolean;
}

/** What `wamn-control:member/list@0.3.0` calls its result members. */
export const MEMBER_LIST_RESULT_FIELDS: FieldMap = {
  "members": {
    member: "members",
    fields: {
      "display_name": "displayName",
      "email": "email",
      "environments": "environments",
      "org_admin": "orgAdmin",
      "principal_id": "principalId",
      "project_admin": "projectAdmin",
    },
  },
};

/**
 * Where the release publishes `wamn-control:member/list@0.3.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const MEMBER_LIST_ROUTE: OperationRoute = {
  operation: "wamn-control:member/list@0.3.0",
  method: "GET",
  template: "/wamn_control/member/list",
  freshOnly: false,
  contract: {
    resultClass: "one",
    partialSchema: null,
    errors: [
      { literal: "invalid_input", required: ["field"], sources: ["malformed_input"], text: null },
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

/** Invoke `wamn-control:member/list@0.3.0` through a transport the application supplies. */
export async function list(
  transport: Transport,
  items: readonly MemberListRequest[],
): Promise<Outcome<MemberListResult>> {
  return reviveOutcome<MemberListResult>(
    await transport.invoke({
      ...MEMBER_LIST_ROUTE,
      items: items.map((item) => toWire(item, MEMBER_LIST_REQUEST_FIELDS)),
    }),
    MEMBER_LIST_RESULT_FIELDS,
  );
}

/** Input for `wamn-control:member/revoke@0.3.0`. */
export interface MemberRevokeRequest {
  /** `text` */
  requestId: string;
  /** `object` */
  value: MemberRevokeRequestValue;
}

export interface MemberRevokeRequestValue {
  /** `text` */
  env: string;
  /** `uuid` */
  principalId: Uuid;
  /** `text` */
  project: string;
}

/** What `wamn-control:member/revoke@0.3.0` calls its input members. */
export const MEMBER_REVOKE_REQUEST_FIELDS: FieldMap = {
  "request_id": "requestId",
  "value": {
    member: "value",
    fields: {
      "env": "env",
      "principal_id": "principalId",
      "project": "project",
    },
  },
};

/** Result of `wamn-control:member/revoke@0.3.0`. */
export interface MemberRevokeResult {
  /** `text` */
  readonly env: string;
  /** `boolean` */
  readonly member: boolean;
  /** `uuid` */
  readonly principalId: Uuid;
  /** `text` */
  readonly project: string;
}

/** What `wamn-control:member/revoke@0.3.0` calls its result members. */
export const MEMBER_REVOKE_RESULT_FIELDS: FieldMap = {
  "env": "env",
  "member": "member",
  "principal_id": "principalId",
  "project": "project",
};

/**
 * Where the release publishes `wamn-control:member/revoke@0.3.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const MEMBER_REVOKE_ROUTE: OperationRoute = {
  operation: "wamn-control:member/revoke@0.3.0",
  method: "POST",
  template: "/wamn_control/member/revoke",
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

/** Invoke `wamn-control:member/revoke@0.3.0` through a transport the application supplies. */
export async function revoke(
  transport: Transport,
  items: readonly MemberRevokeRequest[],
): Promise<Outcome<MemberRevokeResult>> {
  return reviveOutcome<MemberRevokeResult>(
    await transport.invoke({
      ...MEMBER_REVOKE_ROUTE,
      items: items.map((item) => toWire(item, MEMBER_REVOKE_REQUEST_FIELDS)),
    }),
    MEMBER_REVOKE_RESULT_FIELDS,
  );
}
