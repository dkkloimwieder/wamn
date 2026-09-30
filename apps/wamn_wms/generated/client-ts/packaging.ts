// @generated from the client-contract IR; do not edit.
//
// `packaging` operations of package `wamn_wms`.

import type { FieldMap, JsonValue, OperationRoute, Outcome, Timestamptz, Transport, Uuid } from "@wamn/web-runtime";
import { reviveOutcome, toWire } from "@wamn/web-runtime";

/** Input for `wamn-wms:packaging/create@1.0.0`. */
export interface PackagingCreateRequest {
  /** `text` */
  idempotencyKey: string;
  /** `uuid` */
  locationId: Uuid;
  /** `text` */
  packagingCode: string;
  /** `string` */
  requestId: string;
  /** `text` */
  status: "available" | "held";
  /** `text` */
  type: "bin" | "case" | "loose" | "pallet" | "tote";
}

/** What `wamn-wms:packaging/create@1.0.0` calls its input members. */
export const PACKAGING_CREATE_REQUEST_FIELDS: FieldMap = {
  "idempotency_key": "idempotencyKey",
  "location_id": "locationId",
  "packaging_code": "packagingCode",
  "request_id": "requestId",
  "status": "status",
  "type": "type",
};

/** Result of `wamn-wms:packaging/create@1.0.0`. */
export interface PackagingCreateResult {
  /** `timestamptz` */
  readonly createdAt: Timestamptz;
  /** `uuid` */
  readonly createdBy: Uuid;
  /** `uuid` */
  readonly id: Uuid;
  /** `timestamptz` */
  readonly locatedAt: Timestamptz;
  /** `uuid` */
  readonly locationId: Uuid;
  /** `text` */
  readonly packagingCode: string;
  /** `int32` */
  readonly rowVersion: number;
  /** `text` */
  readonly status: "available" | "consumed" | "held";
  /** `text` */
  readonly type: "bin" | "case" | "loose" | "pallet" | "tote";
  /** `timestamptz` */
  readonly updatedAt: Timestamptz;
  /** `uuid` */
  readonly updatedBy: Uuid;
}

/** What `wamn-wms:packaging/create@1.0.0` calls its result members. */
export const PACKAGING_CREATE_RESULT_FIELDS: FieldMap = {
  "created_at": "createdAt",
  "created_by": "createdBy",
  "id": "id",
  "located_at": "locatedAt",
  "location_id": "locationId",
  "packaging_code": "packagingCode",
  "row_version": "rowVersion",
  "status": "status",
  "type": "type",
  "updated_at": "updatedAt",
  "updated_by": "updatedBy",
};

/**
 * Where the release publishes `wamn-wms:packaging/create@1.0.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const PACKAGING_CREATE_ROUTE: OperationRoute = {
  operation: "wamn-wms:packaging/create@1.0.0",
  method: "POST",
  template: "/packaging/create",
  freshOnly: false,
  contract: {
    resultClass: "one",
    partialSchema: null,
    errors: [
      { literal: "check_violation", required: ["constraint"], sources: ["check_violation"], text: null },
      { literal: "foreign_key_violation", required: ["constraint", "field"], sources: ["foreign_key_violation"], text: null },
      { literal: "idempotency_conflict", required: ["field"], sources: ["changed_request"], text: null },
      { literal: "internal_error", required: [], sources: ["query_error", "row_limit_exceeded"], text: null },
      { literal: "invalid_input", required: ["field"], sources: [], text: null },
      { literal: "permission_denied", required: ["operation"], sources: ["permission_denied"], text: null },
      { literal: "retry", required: [], sources: ["connection_unavailable", "serialization_failure"], text: null },
      { literal: "timeout", required: [], sources: ["statement_timeout"], text: null },
      { literal: "unique_violation", required: ["constraint"], sources: ["unique_violation"], text: null },
    ],
    replay: "claim",
    direct: true,
    type: "create",
    transaction: "explicit_per_input",
  },
};

/** Invoke `wamn-wms:packaging/create@1.0.0` through a transport the application supplies. */
export async function create(
  transport: Transport,
  items: readonly PackagingCreateRequest[],
): Promise<Outcome<PackagingCreateResult>> {
  return reviveOutcome<PackagingCreateResult>(
    await transport.invoke({
      ...PACKAGING_CREATE_ROUTE,
      items: items.map((item) => toWire(item, PACKAGING_CREATE_REQUEST_FIELDS)),
    }),
    PACKAGING_CREATE_RESULT_FIELDS,
  );
}

/** Input for `wamn-wms:packaging/get@1.0.0`. */
export interface PackagingGetRequest {
  /** `uuid` */
  id: Uuid;
}

/** What `wamn-wms:packaging/get@1.0.0` calls its input members. */
export const PACKAGING_GET_REQUEST_FIELDS: FieldMap = {
  "id": "id",
};

/** Result of `wamn-wms:packaging/get@1.0.0`. */
export interface PackagingGetResult {
  /** `timestamptz` */
  readonly createdAt: Timestamptz;
  /** `uuid` */
  readonly createdBy: Uuid;
  /** `uuid` */
  readonly id: Uuid;
  /** `timestamptz` */
  readonly locatedAt: Timestamptz;
  /** `uuid` */
  readonly locationId: Uuid;
  /** `text` */
  readonly packagingCode: string;
  /** `int32` */
  readonly rowVersion: number;
  /** `text` */
  readonly status: "available" | "consumed" | "held";
  /** `text` */
  readonly type: "bin" | "case" | "loose" | "pallet" | "tote";
  /** `timestamptz` */
  readonly updatedAt: Timestamptz;
  /** `uuid` */
  readonly updatedBy: Uuid;
}

/** What `wamn-wms:packaging/get@1.0.0` calls its result members. */
export const PACKAGING_GET_RESULT_FIELDS: FieldMap = {
  "created_at": "createdAt",
  "created_by": "createdBy",
  "id": "id",
  "located_at": "locatedAt",
  "location_id": "locationId",
  "packaging_code": "packagingCode",
  "row_version": "rowVersion",
  "status": "status",
  "type": "type",
  "updated_at": "updatedAt",
  "updated_by": "updatedBy",
};

/**
 * Where the release publishes `wamn-wms:packaging/get@1.0.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const PACKAGING_GET_ROUTE: OperationRoute = {
  operation: "wamn-wms:packaging/get@1.0.0",
  method: "GET",
  template: "/packaging/get",
  freshOnly: false,
  contract: {
    resultClass: "one",
    partialSchema: null,
    errors: [
      { literal: "internal_error", required: [], sources: ["query_error", "row_limit_exceeded"], text: null },
      { literal: "invalid_input", required: ["field"], sources: [], text: null },
      { literal: "not_found", required: ["field", "id"], sources: [], text: null },
      { literal: "permission_denied", required: ["operation"], sources: ["permission_denied"], text: null },
      { literal: "retry", required: [], sources: ["connection_unavailable", "serialization_failure"], text: null },
      { literal: "timeout", required: [], sources: ["statement_timeout"], text: null },
    ],
    replay: null,
    direct: true,
    type: "get",
    transaction: "implicit",
  },
};

/** Invoke `wamn-wms:packaging/get@1.0.0` through a transport the application supplies. */
export async function get(
  transport: Transport,
  items: readonly PackagingGetRequest[],
): Promise<Outcome<PackagingGetResult>> {
  return reviveOutcome<PackagingGetResult>(
    await transport.invoke({
      ...PACKAGING_GET_ROUTE,
      items: items.map((item) => toWire(item, PACKAGING_GET_REQUEST_FIELDS)),
    }),
    PACKAGING_GET_RESULT_FIELDS,
  );
}

/** Input for `wamn-wms:packaging/query@1.0.0`. */
export interface PackagingQueryRequest {
  /** `text`, omittable */
  cursor?: string;
  /** `object`, omittable */
  filter?: PackagingQueryRequestFilter;
  /** `int32`, omittable */
  limit?: number;
  /** `keyset`, omittable */
  pagination?: JsonValue;
  /** `object`, omittable */
  sort?: PackagingQueryRequestSort;
}

export interface PackagingQueryRequestFilter {
  /** `array`, omittable */
  locationId?: Uuid[];
  /** `array`, omittable */
  packagingCode?: string[];
  /** `array`, omittable */
  status?: ("available" | "consumed" | "held")[];
}

export interface PackagingQueryRequestSort {
  /** `text` */
  direction: "ascending" | "descending";
  /** `text` */
  field: "created_at" | "location_id" | "packaging_code" | "updated_at";
}

/** What `wamn-wms:packaging/query@1.0.0` calls its input members. */
export const PACKAGING_QUERY_REQUEST_FIELDS: FieldMap = {
  "cursor": "cursor",
  "filter": {
    member: "filter",
    fields: {
      "location_id": "locationId",
      "packaging_code": "packagingCode",
      "status": "status",
    },
  },
  "limit": "limit",
  "pagination": "pagination",
  "sort": {
    member: "sort",
    fields: {
      "direction": "direction",
      "field": "field",
    },
  },
};

/** One row of `wamn-wms:packaging/query@1.0.0`. */
export interface PackagingQueryRow {
  /** `timestamptz` */
  readonly createdAt: Timestamptz;
  /** `uuid` */
  readonly createdBy: Uuid;
  /** `uuid` */
  readonly id: Uuid;
  /** `timestamptz` */
  readonly locatedAt: Timestamptz;
  /** `uuid` */
  readonly locationId: Uuid;
  /** `text` */
  readonly packagingCode: string;
  /** `int32` */
  readonly rowVersion: number;
  /** `text` */
  readonly status: "available" | "consumed" | "held";
  /** `text` */
  readonly type: "bin" | "case" | "loose" | "pallet" | "tote";
  /** `timestamptz` */
  readonly updatedAt: Timestamptz;
  /** `uuid` */
  readonly updatedBy: Uuid;
}

/** Result of `wamn-wms:packaging/query@1.0.0`. */
export interface PackagingQueryResult {
  /** The rows this page carries. */
  readonly item: readonly PackagingQueryRow[];
  /** The next page's cursor, or null at the last page. */
  readonly nextCursor: string | null;
}

/** What `wamn-wms:packaging/query@1.0.0` calls its result members. */
export const PACKAGING_QUERY_RESULT_FIELDS: FieldMap = {
  "item": {
    member: "item",
    fields: {
      "created_at": "createdAt",
      "created_by": "createdBy",
      "id": "id",
      "located_at": "locatedAt",
      "location_id": "locationId",
      "packaging_code": "packagingCode",
      "row_version": "rowVersion",
      "status": "status",
      "type": "type",
      "updated_at": "updatedAt",
      "updated_by": "updatedBy",
    },
  },
  "next_cursor": "nextCursor",
};

/**
 * Where the release publishes `wamn-wms:packaging/query@1.0.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const PACKAGING_QUERY_ROUTE: OperationRoute = {
  operation: "wamn-wms:packaging/query@1.0.0",
  method: "GET",
  template: "/packaging/query",
  freshOnly: false,
  contract: {
    resultClass: "page",
    partialSchema: null,
    errors: [
      { literal: "internal_error", required: [], sources: ["query_error", "row_limit_exceeded"], text: null },
      { literal: "invalid_input", required: ["field"], sources: [], text: null },
      { literal: "permission_denied", required: ["operation"], sources: ["permission_denied"], text: null },
      { literal: "retry", required: [], sources: ["connection_unavailable", "serialization_failure"], text: null },
      { literal: "timeout", required: [], sources: ["statement_timeout"], text: null },
    ],
    replay: null,
    direct: true,
    type: "query",
    transaction: "implicit",
  },
};

/** Invoke `wamn-wms:packaging/query@1.0.0` through a transport the application supplies. */
export async function query(
  transport: Transport,
  items: readonly PackagingQueryRequest[],
): Promise<Outcome<PackagingQueryResult>> {
  return reviveOutcome<PackagingQueryResult>(
    await transport.invoke({
      ...PACKAGING_QUERY_ROUTE,
      items: items.map((item) => toWire(item, PACKAGING_QUERY_REQUEST_FIELDS)),
    }),
    PACKAGING_QUERY_RESULT_FIELDS,
  );
}
