// @generated from the client-contract IR; do not edit.
//
// `control` operations of package `wamn_control`.

import type { FieldMap, OperationRoute, Outcome, Transport } from "@wamn/web-runtime";
import { reviveOutcome, toWire } from "@wamn/web-runtime";

/** Input for `wamn-control:control/mine@0.3.0`. */
export interface ControlMineRequest {
}

/** What `wamn-control:control/mine@0.3.0` calls its input members. */
export const CONTROL_MINE_REQUEST_FIELDS: FieldMap = {};

/** Result of `wamn-control:control/mine@0.3.0`. */
export interface ControlMineResult {
  /** `boolean` */
  readonly orgAdmin: boolean;
  /** `array` */
  readonly projects: readonly ControlMineResultProjects[];
}

export interface ControlMineResultProjects {
  /** `text` */
  readonly project: string;
  /** `boolean` */
  readonly projectAdmin: boolean;
}

/** What `wamn-control:control/mine@0.3.0` calls its result members. */
export const CONTROL_MINE_RESULT_FIELDS: FieldMap = {
  "org_admin": "orgAdmin",
  "projects": {
    member: "projects",
    fields: {
      "project": "project",
      "project_admin": "projectAdmin",
    },
  },
};

/**
 * Where the release publishes `wamn-control:control/mine@0.3.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const CONTROL_MINE_ROUTE: OperationRoute = {
  operation: "wamn-control:control/mine@0.3.0",
  method: "GET",
  template: "/wamn_control/control/mine",
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

/** Invoke `wamn-control:control/mine@0.3.0` through a transport the application supplies. */
export async function mine(
  transport: Transport,
  items: readonly ControlMineRequest[],
): Promise<Outcome<ControlMineResult>> {
  return reviveOutcome<ControlMineResult>(
    await transport.invoke({
      ...CONTROL_MINE_ROUTE,
      items: items.map((item) => toWire(item, CONTROL_MINE_REQUEST_FIELDS)),
    }),
    CONTROL_MINE_RESULT_FIELDS,
  );
}
