// @generated from the client-contract IR; do not edit.
//
// `packaging` operations of package `wamn_wms`.

import type { FieldMap, OperationRoute, Outcome, Timestamptz, Transport, Uuid } from "@wamn/web-runtime";
import { reviveOutcome, toWire } from "@wamn/web-runtime";

/** Input for `wamn-wms:packaging/close@1.0.0`. */
export interface PackagingCloseRequest {
  /** `text` */
  requestId: string;
  /** `object` */
  value: PackagingCloseRequestValue;
}

export interface PackagingCloseRequestValue {
  /** `int32` */
  expectedRowVersion: number;
  /** `text` */
  idempotencyKey: string;
  /** `uuid` */
  packagingId: Uuid;
}

/** What `wamn-wms:packaging/close@1.0.0` calls its input members. */
export const PACKAGING_CLOSE_REQUEST_FIELDS: FieldMap = {
  "request_id": "requestId",
  "value": {
    member: "value",
    fields: {
      "expected_row_version": "expectedRowVersion",
      "idempotency_key": "idempotencyKey",
      "packaging_id": "packagingId",
    },
  },
};

/** Result of `wamn-wms:packaging/close@1.0.0`. */
export interface PackagingCloseResult {
  /** `text` */
  readonly code: string;
  /** `text` */
  readonly lifecycle: string;
  /** `uuid` */
  readonly locationId: Uuid;
  /** `uuid` */
  readonly operationId: Uuid;
  /** `uuid` */
  readonly packagingId: Uuid;
  /** `int32` */
  readonly rowVersion: number;
  /** `text` */
  readonly type: string;
}

/** What `wamn-wms:packaging/close@1.0.0` calls its result members. */
export const PACKAGING_CLOSE_RESULT_FIELDS: FieldMap = {
  "code": "code",
  "lifecycle": "lifecycle",
  "location_id": "locationId",
  "operation_id": "operationId",
  "packaging_id": "packagingId",
  "row_version": "rowVersion",
  "type": "type",
};

/**
 * Where the release publishes `wamn-wms:packaging/close@1.0.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const PACKAGING_CLOSE_ROUTE: OperationRoute = {
  operation: "wamn-wms:packaging/close@1.0.0",
  method: "POST",
  template: "/packaging/close",
  freshOnly: false,
  contract: {
    resultClass: "one",
    partialSchema: null,
    errors: [
      { literal: "concurrency_conflict", required: ["expected_row_version", "observed_row_version"], sources: ["transaction_invariant"] },
      { literal: "idempotency_conflict", required: ["field"], sources: ["same_key_different_canonical_command"] },
      { literal: "internal_error", required: [], sources: ["query_error", "row_limit_exceeded", "undeclared_constraint"] },
      { literal: "invalid_input", required: ["field"], sources: ["envelope_count", "malformed_input"] },
      { literal: "not_found", required: ["field", "id"], sources: ["transaction_invariant"] },
      { literal: "permission_denied", required: ["operation"], sources: ["permission_denied"] },
      { literal: "retry", required: [], sources: ["connection_unavailable", "serialization_failure"] },
      { literal: "timeout", required: [], sources: ["statement_timeout"] },
    ],
    replay: "claim",
    direct: true,
    kind: "command",
    transaction: "explicit_per_input",
  },
};

/** Invoke `wamn-wms:packaging/close@1.0.0` through a transport the application supplies. */
export async function close(
  transport: Transport,
  items: readonly PackagingCloseRequest[],
): Promise<Outcome<PackagingCloseResult>> {
  return reviveOutcome<PackagingCloseResult>(
    await transport.invoke({
      ...PACKAGING_CLOSE_ROUTE,
      items: items.map((item) => toWire(item, PACKAGING_CLOSE_REQUEST_FIELDS)),
    }),
    PACKAGING_CLOSE_RESULT_FIELDS,
  );
}

/** Input for `wamn-wms:packaging/create@1.0.0`. */
export interface PackagingCreateRequest {
  /** `text` */
  requestId: string;
  /** `object` */
  value: PackagingCreateRequestValue;
}

export interface PackagingCreateRequestValue {
  /** `text` */
  code: string;
  /** `text` */
  idempotencyKey: string;
  /** `uuid` */
  locationId: Uuid;
  /** `text` */
  type: string;
}

/** What `wamn-wms:packaging/create@1.0.0` calls its input members. */
export const PACKAGING_CREATE_REQUEST_FIELDS: FieldMap = {
  "request_id": "requestId",
  "value": {
    member: "value",
    fields: {
      "code": "code",
      "idempotency_key": "idempotencyKey",
      "location_id": "locationId",
      "type": "type",
    },
  },
};

/** Result of `wamn-wms:packaging/create@1.0.0`. */
export interface PackagingCreateResult {
  /** `text` */
  readonly code: string;
  /** `text` */
  readonly lifecycle: string;
  /** `uuid` */
  readonly locationId: Uuid;
  /** `uuid` */
  readonly operationId: Uuid;
  /** `uuid` */
  readonly packagingId: Uuid;
  /** `int32` */
  readonly rowVersion: number;
  /** `text` */
  readonly type: string;
}

/** What `wamn-wms:packaging/create@1.0.0` calls its result members. */
export const PACKAGING_CREATE_RESULT_FIELDS: FieldMap = {
  "code": "code",
  "lifecycle": "lifecycle",
  "location_id": "locationId",
  "operation_id": "operationId",
  "packaging_id": "packagingId",
  "row_version": "rowVersion",
  "type": "type",
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
      { literal: "concurrency_conflict", required: ["expected_row_version", "observed_row_version"], sources: ["transaction_invariant"] },
      { literal: "idempotency_conflict", required: ["field"], sources: ["same_key_different_canonical_command"] },
      { literal: "internal_error", required: [], sources: ["query_error", "row_limit_exceeded", "undeclared_constraint"] },
      { literal: "invalid_input", required: ["field"], sources: ["envelope_count", "malformed_input"] },
      { literal: "not_found", required: ["field", "id"], sources: ["transaction_invariant"] },
      { literal: "permission_denied", required: ["operation"], sources: ["permission_denied"] },
      { literal: "retry", required: [], sources: ["connection_unavailable", "serialization_failure"] },
      { literal: "timeout", required: [], sources: ["statement_timeout"] },
    ],
    replay: "claim",
    direct: true,
    kind: "command",
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
  /** `text` */
  readonly code: string;
  /** `timestamptz` */
  readonly createdAt: Timestamptz;
  /** `uuid` */
  readonly id: Uuid;
  /** `text` */
  readonly lifecycle: "closed" | "open";
  /** `uuid` */
  readonly locationId: Uuid;
  /** `int32` */
  readonly rowVersion: number;
  /** `text` */
  readonly type: string;
}

/** What `wamn-wms:packaging/get@1.0.0` calls its result members. */
export const PACKAGING_GET_RESULT_FIELDS: FieldMap = {
  "code": "code",
  "created_at": "createdAt",
  "id": "id",
  "lifecycle": "lifecycle",
  "location_id": "locationId",
  "row_version": "rowVersion",
  "type": "type",
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
  /** `int32`, omittable */
  limit?: number;
}

/** What `wamn-wms:packaging/query@1.0.0` calls its input members. */
export const PACKAGING_QUERY_REQUEST_FIELDS: FieldMap = {
  "cursor": "cursor",
  "limit": "limit",
};

/** One row of `wamn-wms:packaging/query@1.0.0`. */
export interface PackagingQueryRow {
  /** `text` */
  readonly code: string;
  /** `timestamptz` */
  readonly createdAt: Timestamptz;
  /** `uuid` */
  readonly id: Uuid;
  /** `text` */
  readonly lifecycle: "closed" | "open";
  /** `uuid` */
  readonly locationId: Uuid;
  /** `int32` */
  readonly rowVersion: number;
  /** `text` */
  readonly type: string;
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
      "code": "code",
      "created_at": "createdAt",
      "id": "id",
      "lifecycle": "lifecycle",
      "location_id": "locationId",
      "row_version": "rowVersion",
      "type": "type",
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

/** Input for `wamn-wms:packaging/relocate@1.0.0`. */
export interface PackagingRelocateRequest {
  /** `text` */
  requestId: string;
  /** `object` */
  value: PackagingRelocateRequestValue;
}

export interface PackagingRelocateRequestValue {
  /** `int32` */
  expectedRowVersion: number;
  /** `text` */
  idempotencyKey: string;
  /** `timestamptz` */
  occurredAt: Timestamptz;
  /** `uuid` */
  packagingId: Uuid;
  /** `uuid` */
  toLocationId: Uuid;
}

/** What `wamn-wms:packaging/relocate@1.0.0` calls its input members. */
export const PACKAGING_RELOCATE_REQUEST_FIELDS: FieldMap = {
  "request_id": "requestId",
  "value": {
    member: "value",
    fields: {
      "expected_row_version": "expectedRowVersion",
      "idempotency_key": "idempotencyKey",
      "occurred_at": "occurredAt",
      "packaging_id": "packagingId",
      "to_location_id": "toLocationId",
    },
  },
};

/** Result of `wamn-wms:packaging/relocate@1.0.0`. */
export interface PackagingRelocateResult {
  /** `text` */
  readonly code: string;
  /** `text` */
  readonly lifecycle: string;
  /** `uuid` */
  readonly locationId: Uuid;
  /** `uuid` */
  readonly operationId: Uuid;
  /** `uuid` */
  readonly packagingId: Uuid;
  /** `int32` */
  readonly rowVersion: number;
  /** `text` */
  readonly type: string;
}

/** What `wamn-wms:packaging/relocate@1.0.0` calls its result members. */
export const PACKAGING_RELOCATE_RESULT_FIELDS: FieldMap = {
  "code": "code",
  "lifecycle": "lifecycle",
  "location_id": "locationId",
  "operation_id": "operationId",
  "packaging_id": "packagingId",
  "row_version": "rowVersion",
  "type": "type",
};

/**
 * Where the release publishes `wamn-wms:packaging/relocate@1.0.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const PACKAGING_RELOCATE_ROUTE: OperationRoute = {
  operation: "wamn-wms:packaging/relocate@1.0.0",
  method: "POST",
  template: "/packaging/relocate",
  freshOnly: false,
  contract: {
    resultClass: "one",
    partialSchema: null,
    errors: [
      { literal: "concurrency_conflict", required: ["expected_row_version", "observed_row_version"], sources: ["transaction_invariant"] },
      { literal: "idempotency_conflict", required: ["field"], sources: ["same_key_different_canonical_command"] },
      { literal: "internal_error", required: [], sources: ["query_error", "row_limit_exceeded", "undeclared_constraint"] },
      { literal: "invalid_input", required: ["field"], sources: ["envelope_count", "malformed_input"] },
      { literal: "not_found", required: ["field", "id"], sources: ["transaction_invariant"] },
      { literal: "permission_denied", required: ["operation"], sources: ["permission_denied"] },
      { literal: "retry", required: [], sources: ["connection_unavailable", "serialization_failure"] },
      { literal: "timeout", required: [], sources: ["statement_timeout"] },
    ],
    replay: "claim",
    direct: true,
    kind: "command",
    transaction: "explicit_per_input",
  },
};

/** Invoke `wamn-wms:packaging/relocate@1.0.0` through a transport the application supplies. */
export async function relocate(
  transport: Transport,
  items: readonly PackagingRelocateRequest[],
): Promise<Outcome<PackagingRelocateResult>> {
  return reviveOutcome<PackagingRelocateResult>(
    await transport.invoke({
      ...PACKAGING_RELOCATE_ROUTE,
      items: items.map((item) => toWire(item, PACKAGING_RELOCATE_REQUEST_FIELDS)),
    }),
    PACKAGING_RELOCATE_RESULT_FIELDS,
  );
}
