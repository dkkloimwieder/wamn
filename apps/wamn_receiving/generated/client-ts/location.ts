// @generated from the client-contract IR; do not edit.
//
// `location` operations of package `wamn_receiving`.

import type { OperationRoute, Outcome, Transport, Uuid } from "./wire.js";
import { reviveOutcome, toWire } from "./wire.js";

/** Input for `wamn-receiving:location/list@1.0.0`. */
export interface LocationListRequest {
  /** `text` */
  readonly requestId: string;
}

/** Result of `wamn-receiving:location/list@1.0.0`. */
export interface LocationListResult {
  /** `uuid` */
  readonly id: Uuid;
  /** `text` */
  readonly locationCode: string;
}

/**
 * Where the release publishes `wamn-receiving:location/list@1.0.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const LOCATION_LIST_ROUTE: OperationRoute = {
  operation: "wamn-receiving:location/list@1.0.0",
  method: "POST",
  template: "/location/list",
  freshOnly: false,
  contract: {
    resultClass: "bounded_list",
    partialSchema: null,
    errors: [
      "internal_error",
      "invalid_input",
      "permission_denied",
      "retry",
      "timeout",
    ],
    replay: null,
  },
};

/** Invoke `wamn-receiving:location/list@1.0.0` through a transport the application supplies. */
export async function list(
  transport: Transport,
  items: readonly LocationListRequest[],
): Promise<Outcome<LocationListResult>> {
  return reviveOutcome<LocationListResult>(
    await transport.invoke({ ...LOCATION_LIST_ROUTE, items: items.map(toWire) }),
  );
}
