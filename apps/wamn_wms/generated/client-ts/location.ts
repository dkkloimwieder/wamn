// @generated from the client-contract IR; do not edit.
//
// `location` operations of package `wamn_wms`.

import type { FieldMap, OperationRoute, Outcome, Timestamptz, Transport, Uuid } from "@wamn/web-runtime";
import { reviveOutcome, toWire } from "@wamn/web-runtime";

/** Input for `wamn-wms:location/create@1.0.0`. */
export interface LocationCreateRequest {
  /** `text` */
  idempotencyKey: string;
  /** `text` */
  locationCode: string;
  /** `string` */
  requestId: string;
}

/** What `wamn-wms:location/create@1.0.0` calls its input members. */
export const LOCATION_CREATE_REQUEST_FIELDS: FieldMap = {
  "idempotency_key": "idempotencyKey",
  "location_code": "locationCode",
  "request_id": "requestId",
};

/** Result of `wamn-wms:location/create@1.0.0`. */
export interface LocationCreateResult {
  /** `timestamptz` */
  readonly createdAt: Timestamptz;
  /** `uuid` */
  readonly id: Uuid;
  /** `text` */
  readonly locationCode: string;
  /** `int32` */
  readonly rowVersion: number;
}

/** What `wamn-wms:location/create@1.0.0` calls its result members. */
export const LOCATION_CREATE_RESULT_FIELDS: FieldMap = {
  "created_at": "createdAt",
  "id": "id",
  "location_code": "locationCode",
  "row_version": "rowVersion",
};

/**
 * Where the release publishes `wamn-wms:location/create@1.0.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const LOCATION_CREATE_ROUTE: OperationRoute = {
  operation: "wamn-wms:location/create@1.0.0",
  method: "POST",
  template: "/location/create",
  freshOnly: false,
  contract: {
    resultClass: "one",
    partialSchema: null,
    errors: [
      { literal: "idempotency_conflict", required: ["field"], sources: ["changed_canonical_command"] },
      { literal: "internal_error", required: [], sources: ["query_error", "row_limit_exceeded"] },
      { literal: "invalid_input", required: ["field"], sources: [] },
      { literal: "permission_denied", required: ["operation"], sources: ["permission_denied"] },
      { literal: "retry", required: [], sources: ["connection_unavailable", "serialization_failure"] },
      { literal: "timeout", required: [], sources: ["statement_timeout"] },
      { literal: "unique_violation", required: ["constraint"], sources: ["unique_violation"] },
    ],
    replay: "claim",
    direct: true,
    kind: "create",
    transaction: "explicit_per_input",
  },
};

/** Invoke `wamn-wms:location/create@1.0.0` through a transport the application supplies. */
export async function create(
  transport: Transport,
  items: readonly LocationCreateRequest[],
): Promise<Outcome<LocationCreateResult>> {
  return reviveOutcome<LocationCreateResult>(
    await transport.invoke({
      ...LOCATION_CREATE_ROUTE,
      items: items.map((item) => toWire(item, LOCATION_CREATE_REQUEST_FIELDS)),
    }),
    LOCATION_CREATE_RESULT_FIELDS,
  );
}

/** Input for `wamn-wms:location/get@1.0.0`. */
export interface LocationGetRequest {
  /** `uuid` */
  id: Uuid;
  /** `string` */
  requestId: string;
}

/** What `wamn-wms:location/get@1.0.0` calls its input members. */
export const LOCATION_GET_REQUEST_FIELDS: FieldMap = {
  "id": "id",
  "request_id": "requestId",
};

/** Result of `wamn-wms:location/get@1.0.0`. */
export interface LocationGetResult {
  /** `timestamptz` */
  readonly createdAt: Timestamptz;
  /** `uuid` */
  readonly id: Uuid;
  /** `text` */
  readonly locationCode: string;
  /** `int32` */
  readonly rowVersion: number;
}

/** What `wamn-wms:location/get@1.0.0` calls its result members. */
export const LOCATION_GET_RESULT_FIELDS: FieldMap = {
  "created_at": "createdAt",
  "id": "id",
  "location_code": "locationCode",
  "row_version": "rowVersion",
};

/**
 * Where the release publishes `wamn-wms:location/get@1.0.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const LOCATION_GET_ROUTE: OperationRoute = {
  operation: "wamn-wms:location/get@1.0.0",
  method: "POST",
  template: "/location/get",
  freshOnly: false,
  contract: {
    resultClass: "one",
    partialSchema: null,
    errors: [
      { literal: "internal_error", required: [], sources: ["query_error", "row_limit_exceeded"] },
      { literal: "invalid_input", required: ["field"], sources: [] },
      { literal: "not_found", required: ["field", "id"], sources: [] },
      { literal: "permission_denied", required: ["operation"], sources: ["permission_denied"] },
      { literal: "retry", required: [], sources: ["connection_unavailable", "serialization_failure"] },
      { literal: "timeout", required: [], sources: ["statement_timeout"] },
    ],
    replay: null,
    direct: true,
    kind: "get",
    transaction: "implicit",
  },
};

/** Invoke `wamn-wms:location/get@1.0.0` through a transport the application supplies. */
export async function get(
  transport: Transport,
  items: readonly LocationGetRequest[],
): Promise<Outcome<LocationGetResult>> {
  return reviveOutcome<LocationGetResult>(
    await transport.invoke({
      ...LOCATION_GET_ROUTE,
      items: items.map((item) => toWire(item, LOCATION_GET_REQUEST_FIELDS)),
    }),
    LOCATION_GET_RESULT_FIELDS,
  );
}

/** Input for `wamn-wms:location/query@1.0.0`. */
export interface LocationQueryRequest {
  /** `text`, omittable */
  cursor?: string;
  /** `object`, omittable */
  filter?: LocationQueryRequestFilter;
  /** `int32`, omittable */
  limit?: number;
  /** `string` */
  requestId: string;
}

export interface LocationQueryRequestFilter {
  /** `array`, omittable */
  locationCode?: string[];
}

/** What `wamn-wms:location/query@1.0.0` calls its input members. */
export const LOCATION_QUERY_REQUEST_FIELDS: FieldMap = {
  "cursor": "cursor",
  "filter": {
    member: "filter",
    fields: {
      "location_code": "locationCode",
    },
  },
  "limit": "limit",
  "request_id": "requestId",
};

/** One row of `wamn-wms:location/query@1.0.0`. */
export interface LocationQueryRow {
  /** `timestamptz` */
  readonly createdAt: Timestamptz;
  /** `uuid` */
  readonly id: Uuid;
  /** `text` */
  readonly locationCode: string;
  /** `int32` */
  readonly rowVersion: number;
}

/** Result of `wamn-wms:location/query@1.0.0`. */
export interface LocationQueryResult {
  /** The rows this page carries. */
  readonly item: readonly LocationQueryRow[];
  /** The next page's cursor, or null at the last page. */
  readonly nextCursor: string | null;
}

/** What `wamn-wms:location/query@1.0.0` calls its result members. */
export const LOCATION_QUERY_RESULT_FIELDS: FieldMap = {
  "item": {
    member: "item",
    fields: {
      "created_at": "createdAt",
      "id": "id",
      "location_code": "locationCode",
      "row_version": "rowVersion",
    },
  },
  "next_cursor": "nextCursor",
};

/**
 * Where the release publishes `wamn-wms:location/query@1.0.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const LOCATION_QUERY_ROUTE: OperationRoute = {
  operation: "wamn-wms:location/query@1.0.0",
  method: "POST",
  template: "/location/query",
  freshOnly: false,
  contract: {
    resultClass: "page",
    partialSchema: null,
    errors: [
      { literal: "internal_error", required: [], sources: ["query_error", "row_limit_exceeded"] },
      { literal: "invalid_input", required: ["field"], sources: [] },
      { literal: "permission_denied", required: ["operation"], sources: ["permission_denied"] },
      { literal: "retry", required: [], sources: ["connection_unavailable", "serialization_failure"] },
      { literal: "timeout", required: [], sources: ["statement_timeout"] },
    ],
    replay: null,
    direct: true,
    kind: "query",
    transaction: "implicit",
  },
};

/** Invoke `wamn-wms:location/query@1.0.0` through a transport the application supplies. */
export async function query(
  transport: Transport,
  items: readonly LocationQueryRequest[],
): Promise<Outcome<LocationQueryResult>> {
  return reviveOutcome<LocationQueryResult>(
    await transport.invoke({
      ...LOCATION_QUERY_ROUTE,
      items: items.map((item) => toWire(item, LOCATION_QUERY_REQUEST_FIELDS)),
    }),
    LOCATION_QUERY_RESULT_FIELDS,
  );
}

/** Input for `wamn-wms:location/update@1.0.0`. */
export interface LocationUpdateRequest {
  /** `object` */
  change: LocationUpdateRequestChange;
  /** `int32` */
  expectedRowVersion: number;
  /** `uuid` */
  id: Uuid;
  /** `string` */
  requestId: string;
}

export interface LocationUpdateRequestChange {
  /** `text`, omittable */
  locationCode?: string;
}

/** What `wamn-wms:location/update@1.0.0` calls its input members. */
export const LOCATION_UPDATE_REQUEST_FIELDS: FieldMap = {
  "change": {
    member: "change",
    fields: {
      "location_code": "locationCode",
    },
  },
  "expected_row_version": "expectedRowVersion",
  "id": "id",
  "request_id": "requestId",
};

/** Result of `wamn-wms:location/update@1.0.0`. */
export interface LocationUpdateResult {
  /** `timestamptz` */
  readonly createdAt: Timestamptz;
  /** `uuid` */
  readonly id: Uuid;
  /** `text` */
  readonly locationCode: string;
  /** `int32` */
  readonly rowVersion: number;
}

/** What `wamn-wms:location/update@1.0.0` calls its result members. */
export const LOCATION_UPDATE_RESULT_FIELDS: FieldMap = {
  "created_at": "createdAt",
  "id": "id",
  "location_code": "locationCode",
  "row_version": "rowVersion",
};

/**
 * Where the release publishes `wamn-wms:location/update@1.0.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const LOCATION_UPDATE_ROUTE: OperationRoute = {
  operation: "wamn-wms:location/update@1.0.0",
  method: "POST",
  template: "/location/update",
  freshOnly: false,
  contract: {
    resultClass: "one",
    partialSchema: null,
    errors: [
      { literal: "concurrency_conflict", required: ["expected_row_version", "observed_row_version"], sources: [] },
      { literal: "internal_error", required: [], sources: ["query_error", "row_limit_exceeded"] },
      { literal: "invalid_input", required: ["field"], sources: [] },
      { literal: "not_found", required: ["field", "id"], sources: [] },
      { literal: "permission_denied", required: ["operation"], sources: ["permission_denied"] },
      { literal: "retry", required: [], sources: ["connection_unavailable", "serialization_failure"] },
      { literal: "timeout", required: [], sources: ["statement_timeout"] },
      { literal: "unique_violation", required: ["constraint"], sources: ["unique_violation"] },
    ],
    replay: null,
    direct: true,
    kind: "update",
    transaction: "implicit",
  },
};

/** Invoke `wamn-wms:location/update@1.0.0` through a transport the application supplies. */
export async function update(
  transport: Transport,
  items: readonly LocationUpdateRequest[],
): Promise<Outcome<LocationUpdateResult>> {
  return reviveOutcome<LocationUpdateResult>(
    await transport.invoke({
      ...LOCATION_UPDATE_ROUTE,
      items: items.map((item) => toWire(item, LOCATION_UPDATE_REQUEST_FIELDS)),
    }),
    LOCATION_UPDATE_RESULT_FIELDS,
  );
}
