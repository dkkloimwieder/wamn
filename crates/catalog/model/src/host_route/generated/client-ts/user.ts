// @generated from the client-contract IR; do not edit.
//
// `user` operations of package `wamn_control`.

import type { FieldMap, OperationRoute, Outcome, Transport, Uuid } from "@wamn/web-runtime";
import { reviveOutcome, toWire } from "@wamn/web-runtime";

/** Input for `wamn-control:user/list@0.2.0`. */
export interface UserListRequest {
}

/** What `wamn-control:user/list@0.2.0` calls its input members. */
export const USER_LIST_REQUEST_FIELDS: FieldMap = {};

/** Result of `wamn-control:user/list@0.2.0`. */
export interface UserListResult {
  /** `array` */
  readonly users: readonly UserListResultUsers[];
}

export interface UserListResultUsers {
  /** `text` */
  readonly displayName: string | null;
  /** `text` */
  readonly email: string;
  /** `uuid` */
  readonly id: Uuid;
  /** `array` */
  readonly roles: readonly string[];
}

/** What `wamn-control:user/list@0.2.0` calls its result members. */
export const USER_LIST_RESULT_FIELDS: FieldMap = {
  "users": {
    member: "users",
    fields: {
      "display_name": "displayName",
      "email": "email",
      "id": "id",
      "roles": "roles",
    },
  },
};

/**
 * Where the release publishes `wamn-control:user/list@0.2.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const USER_LIST_ROUTE: OperationRoute = {
  operation: "wamn-control:user/list@0.2.0",
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

/** Invoke `wamn-control:user/list@0.2.0` through a transport the application supplies. */
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
