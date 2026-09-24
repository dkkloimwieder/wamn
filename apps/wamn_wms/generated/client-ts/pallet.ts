// @generated from the client-contract IR; do not edit.
//
// `pallet` operations of package `wamn_wms`.

import type { FieldMap, OperationRoute, Outcome, Timestamptz, Transport, Uuid } from "@wamn/web-runtime";
import { reviveOutcome, toWire } from "@wamn/web-runtime";

/** Input for `wamn-wms:pallet/create@1.0.0`. */
export interface PalletCreateRequest {
  /** `text` */
  idempotencyKey: string;
  /** `uuid` */
  locationId: Uuid;
  /** `text` */
  palletCode: string;
  /** `string` */
  requestId: string;
  /** `text` */
  status: "available" | "held";
}

/** What `wamn-wms:pallet/create@1.0.0` calls its input members. */
export const PALLET_CREATE_REQUEST_FIELDS: FieldMap = {
  "idempotency_key": "idempotencyKey",
  "location_id": "locationId",
  "pallet_code": "palletCode",
  "request_id": "requestId",
  "status": "status",
};

/** Result of `wamn-wms:pallet/create@1.0.0`. */
export interface PalletCreateResult {
  /** `timestamptz` */
  readonly createdAt: Timestamptz;
  /** `uuid` */
  readonly createdBy: Uuid;
  /** `uuid` */
  readonly id: Uuid;
  /** `uuid` */
  readonly locationId: Uuid;
  /** `text` */
  readonly palletCode: string;
  /** `int32` */
  readonly rowVersion: number;
  /** `text` */
  readonly status: "available" | "consumed" | "held";
  /** `timestamptz` */
  readonly updatedAt: Timestamptz;
  /** `uuid` */
  readonly updatedBy: Uuid;
}

/** What `wamn-wms:pallet/create@1.0.0` calls its result members. */
export const PALLET_CREATE_RESULT_FIELDS: FieldMap = {
  "created_at": "createdAt",
  "created_by": "createdBy",
  "id": "id",
  "location_id": "locationId",
  "pallet_code": "palletCode",
  "row_version": "rowVersion",
  "status": "status",
  "updated_at": "updatedAt",
  "updated_by": "updatedBy",
};

/**
 * Where the release publishes `wamn-wms:pallet/create@1.0.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const PALLET_CREATE_ROUTE: OperationRoute = {
  operation: "wamn-wms:pallet/create@1.0.0",
  method: "POST",
  template: "/pallet/create",
  freshOnly: false,
  contract: {
    resultClass: "one",
    partialSchema: null,
    errors: [
      { literal: "check_violation", required: ["constraint"], sources: ["check_violation"] },
      { literal: "foreign_key_violation", required: ["constraint"], sources: ["foreign_key_violation"] },
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

/** Invoke `wamn-wms:pallet/create@1.0.0` through a transport the application supplies. */
export async function create(
  transport: Transport,
  items: readonly PalletCreateRequest[],
): Promise<Outcome<PalletCreateResult>> {
  return reviveOutcome<PalletCreateResult>(
    await transport.invoke({
      ...PALLET_CREATE_ROUTE,
      items: items.map((item) => toWire(item, PALLET_CREATE_REQUEST_FIELDS)),
    }),
    PALLET_CREATE_RESULT_FIELDS,
  );
}

/** Input for `wamn-wms:pallet/get@1.0.0`. */
export interface PalletGetRequest {
  /** `uuid` */
  id: Uuid;
  /** `string` */
  requestId: string;
}

/** What `wamn-wms:pallet/get@1.0.0` calls its input members. */
export const PALLET_GET_REQUEST_FIELDS: FieldMap = {
  "id": "id",
  "request_id": "requestId",
};

/** Result of `wamn-wms:pallet/get@1.0.0`. */
export interface PalletGetResult {
  /** `timestamptz` */
  readonly createdAt: Timestamptz;
  /** `uuid` */
  readonly createdBy: Uuid;
  /** `uuid` */
  readonly id: Uuid;
  /** `uuid` */
  readonly locationId: Uuid;
  /** `text` */
  readonly palletCode: string;
  /** `int32` */
  readonly rowVersion: number;
  /** `text` */
  readonly status: "available" | "consumed" | "held";
  /** `timestamptz` */
  readonly updatedAt: Timestamptz;
  /** `uuid` */
  readonly updatedBy: Uuid;
}

/** What `wamn-wms:pallet/get@1.0.0` calls its result members. */
export const PALLET_GET_RESULT_FIELDS: FieldMap = {
  "created_at": "createdAt",
  "created_by": "createdBy",
  "id": "id",
  "location_id": "locationId",
  "pallet_code": "palletCode",
  "row_version": "rowVersion",
  "status": "status",
  "updated_at": "updatedAt",
  "updated_by": "updatedBy",
};

/**
 * Where the release publishes `wamn-wms:pallet/get@1.0.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const PALLET_GET_ROUTE: OperationRoute = {
  operation: "wamn-wms:pallet/get@1.0.0",
  method: "POST",
  template: "/pallet/get",
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

/** Invoke `wamn-wms:pallet/get@1.0.0` through a transport the application supplies. */
export async function get(
  transport: Transport,
  items: readonly PalletGetRequest[],
): Promise<Outcome<PalletGetResult>> {
  return reviveOutcome<PalletGetResult>(
    await transport.invoke({
      ...PALLET_GET_ROUTE,
      items: items.map((item) => toWire(item, PALLET_GET_REQUEST_FIELDS)),
    }),
    PALLET_GET_RESULT_FIELDS,
  );
}

/** Input for `wamn-wms:pallet/query@1.0.0`. */
export interface PalletQueryRequest {
  /** `text`, omittable */
  cursor?: string;
  /** `object`, omittable */
  filter?: PalletQueryRequestFilter;
  /** `int32`, omittable */
  limit?: number;
  /** `string` */
  requestId: string;
  /** `object`, omittable */
  sort?: PalletQueryRequestSort;
}

export interface PalletQueryRequestFilter {
  /** `array`, omittable */
  locationId?: Uuid[];
  /** `array`, omittable */
  palletCode?: string[];
  /** `array`, omittable */
  status?: ("available" | "consumed" | "held")[];
}

export interface PalletQueryRequestSort {
  /** `text` */
  direction: "ascending" | "descending";
  /** `text` */
  field: "created_at" | "location_id" | "pallet_code" | "updated_at";
}

/** What `wamn-wms:pallet/query@1.0.0` calls its input members. */
export const PALLET_QUERY_REQUEST_FIELDS: FieldMap = {
  "cursor": "cursor",
  "filter": {
    member: "filter",
    fields: {
      "location_id": "locationId",
      "pallet_code": "palletCode",
      "status": "status",
    },
  },
  "limit": "limit",
  "request_id": "requestId",
  "sort": {
    member: "sort",
    fields: {
      "direction": "direction",
      "field": "field",
    },
  },
};

/** One row of `wamn-wms:pallet/query@1.0.0`. */
export interface PalletQueryRow {
  /** `timestamptz` */
  readonly createdAt: Timestamptz;
  /** `uuid` */
  readonly createdBy: Uuid;
  /** `uuid` */
  readonly id: Uuid;
  /** `uuid` */
  readonly locationId: Uuid;
  /** `text` */
  readonly palletCode: string;
  /** `int32` */
  readonly rowVersion: number;
  /** `text` */
  readonly status: "available" | "consumed" | "held";
  /** `timestamptz` */
  readonly updatedAt: Timestamptz;
  /** `uuid` */
  readonly updatedBy: Uuid;
}

/** Result of `wamn-wms:pallet/query@1.0.0`. */
export interface PalletQueryResult {
  /** The rows this page carries. */
  readonly item: readonly PalletQueryRow[];
  /** The next page's cursor, or null at the last page. */
  readonly nextCursor: string | null;
}

/** What `wamn-wms:pallet/query@1.0.0` calls its result members. */
export const PALLET_QUERY_RESULT_FIELDS: FieldMap = {
  "item": {
    member: "item",
    fields: {
      "created_at": "createdAt",
      "created_by": "createdBy",
      "id": "id",
      "location_id": "locationId",
      "pallet_code": "palletCode",
      "row_version": "rowVersion",
      "status": "status",
      "updated_at": "updatedAt",
      "updated_by": "updatedBy",
    },
  },
  "next_cursor": "nextCursor",
};

/**
 * Where the release publishes `wamn-wms:pallet/query@1.0.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const PALLET_QUERY_ROUTE: OperationRoute = {
  operation: "wamn-wms:pallet/query@1.0.0",
  method: "POST",
  template: "/pallet/query",
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

/** Invoke `wamn-wms:pallet/query@1.0.0` through a transport the application supplies. */
export async function query(
  transport: Transport,
  items: readonly PalletQueryRequest[],
): Promise<Outcome<PalletQueryResult>> {
  return reviveOutcome<PalletQueryResult>(
    await transport.invoke({
      ...PALLET_QUERY_ROUTE,
      items: items.map((item) => toWire(item, PALLET_QUERY_REQUEST_FIELDS)),
    }),
    PALLET_QUERY_RESULT_FIELDS,
  );
}
