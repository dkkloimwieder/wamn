// @generated from the client-contract IR; do not edit.
//
// `location` operations of package `wamn_receiving`.

import type { FieldMap, OperationRoute, Outcome, Transport, Uuid } from "@wamn/web-runtime";
import { reviveOutcome, toWire } from "@wamn/web-runtime";

/** Input for `wamn-receiving:location/list@1.0.0`. */
export interface LocationListRequest {
  /** `text` */
  readonly requestId: string;
}

/** What `wamn-receiving:location/list@1.0.0` calls its input members. */
export const LOCATION_LIST_REQUEST_FIELDS: FieldMap = {
  "request_id": "requestId",
};

/** One row of `wamn-receiving:location/list@1.0.0`. */
export interface LocationListRow {
  /** `uuid` */
  readonly id: Uuid;
  /** `text` */
  readonly locationCode: string;
}

/** Result of `wamn-receiving:location/list@1.0.0`. */
export interface LocationListResult {
  /** Every row the release served. */
  readonly rows: readonly LocationListRow[];
}

/** What `wamn-receiving:location/list@1.0.0` calls its result members. */
export const LOCATION_LIST_RESULT_FIELDS: FieldMap = {
  "rows": {
    member: "rows",
    fields: {
      "id": "id",
      "location_code": "locationCode",
    },
  },
};

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
      { literal: "internal_error", required: [], sources: ["query_error", "row_limit_exceeded", "undeclared_constraint"] },
      { literal: "invalid_input", required: ["field"], sources: ["malformed_input"] },
      { literal: "permission_denied", required: ["operation"], sources: ["permission_denied"] },
      { literal: "retry", required: [], sources: ["connection_unavailable", "serialization_failure"] },
      { literal: "timeout", required: [], sources: ["statement_timeout"] },
    ],
    replay: null,
    direct: true,
    kind: "projection",
    transaction: null,
  },
};

/** Invoke `wamn-receiving:location/list@1.0.0` through a transport the application supplies. */
export async function list(
  transport: Transport,
  items: readonly LocationListRequest[],
): Promise<Outcome<LocationListResult>> {
  return reviveOutcome<LocationListResult>(
    await transport.invoke({
      ...LOCATION_LIST_ROUTE,
      items: items.map((item) => toWire(item, LOCATION_LIST_REQUEST_FIELDS)),
    }),
    LOCATION_LIST_RESULT_FIELDS,
  );
}
