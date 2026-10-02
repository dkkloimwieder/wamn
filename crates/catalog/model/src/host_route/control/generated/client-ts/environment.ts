// @generated from the client-contract IR; do not edit.
//
// `environment` operations of package `wamn_control`.

import type { FieldMap, OperationRoute, Outcome, Transport } from "@wamn/web-runtime";
import { reviveOutcome, toWire } from "@wamn/web-runtime";

/** Input for `wamn-control:environment/list@0.1.0`. */
export interface EnvironmentListRequest {
  /** `text` */
  project: string;
}

/** What `wamn-control:environment/list@0.1.0` calls its input members. */
export const ENVIRONMENT_LIST_REQUEST_FIELDS: FieldMap = {
  "project": "project",
};

/** Result of `wamn-control:environment/list@0.1.0`. */
export interface EnvironmentListResult {
  /** `array` */
  readonly environments: readonly string[];
}

/** What `wamn-control:environment/list@0.1.0` calls its result members. */
export const ENVIRONMENT_LIST_RESULT_FIELDS: FieldMap = {
  "environments": "environments",
};

/**
 * Where the release publishes `wamn-control:environment/list@0.1.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const ENVIRONMENT_LIST_ROUTE: OperationRoute = {
  operation: "wamn-control:environment/list@0.1.0",
  method: "GET",
  template: "/wamn_control/environment/list",
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

/** Invoke `wamn-control:environment/list@0.1.0` through a transport the application supplies. */
export async function list(
  transport: Transport,
  items: readonly EnvironmentListRequest[],
): Promise<Outcome<EnvironmentListResult>> {
  return reviveOutcome<EnvironmentListResult>(
    await transport.invoke({
      ...ENVIRONMENT_LIST_ROUTE,
      items: items.map((item) => toWire(item, ENVIRONMENT_LIST_REQUEST_FIELDS)),
    }),
    ENVIRONMENT_LIST_RESULT_FIELDS,
  );
}
