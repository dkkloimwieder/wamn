// @generated from the client-contract IR; do not edit.
//
// `project` operations of package `wamn_control`.

import type { FieldMap, OperationRoute, Outcome, Transport } from "@wamn/web-runtime";
import { reviveOutcome, toWire } from "@wamn/web-runtime";

/** Input for `wamn-control:project/list@0.2.0`. */
export interface ProjectListRequest {
}

/** What `wamn-control:project/list@0.2.0` calls its input members. */
export const PROJECT_LIST_REQUEST_FIELDS: FieldMap = {};

/** Result of `wamn-control:project/list@0.2.0`. */
export interface ProjectListResult {
  /** `array` */
  readonly projects: readonly string[];
}

/** What `wamn-control:project/list@0.2.0` calls its result members. */
export const PROJECT_LIST_RESULT_FIELDS: FieldMap = {
  "projects": "projects",
};

/**
 * Where the release publishes `wamn-control:project/list@0.2.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const PROJECT_LIST_ROUTE: OperationRoute = {
  operation: "wamn-control:project/list@0.2.0",
  method: "GET",
  template: "/wamn_control/project/list",
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

/** Invoke `wamn-control:project/list@0.2.0` through a transport the application supplies. */
export async function list(
  transport: Transport,
  items: readonly ProjectListRequest[],
): Promise<Outcome<ProjectListResult>> {
  return reviveOutcome<ProjectListResult>(
    await transport.invoke({
      ...PROJECT_LIST_ROUTE,
      items: items.map((item) => toWire(item, PROJECT_LIST_REQUEST_FIELDS)),
    }),
    PROJECT_LIST_RESULT_FIELDS,
  );
}
