// @generated from the client-contract IR; do not edit.
//
// `inventory` operations of package `wamn_wms`.

import type { FieldMap, Numeric, OperationRoute, Outcome, Timestamptz, Transport, Uuid } from "@wamn/web-runtime";
import { reviveOutcome, toWire } from "@wamn/web-runtime";

/** Input for `wamn-wms:inventory/adjust@1.0.0`. */
export interface InventoryAdjustRequest {
  /** `text` */
  requestId: string;
  /** `object` */
  value: InventoryAdjustRequestValue;
}

export interface InventoryAdjustRequestValue {
  /** `int32` */
  expectedRowVersion: number;
  /** `text` */
  idempotencyKey: string;
  /** `uuid` */
  inventoryId: Uuid;
  /** `timestamptz` */
  occurredAt: Timestamptz;
  /** `text` */
  reason: string;
  /** `numeric` */
  toQuantity: Numeric;
}

/** What `wamn-wms:inventory/adjust@1.0.0` calls its input members. */
export const INVENTORY_ADJUST_REQUEST_FIELDS: FieldMap = {
  "request_id": "requestId",
  "value": {
    member: "value",
    fields: {
      "expected_row_version": "expectedRowVersion",
      "idempotency_key": "idempotencyKey",
      "inventory_id": "inventoryId",
      "occurred_at": "occurredAt",
      "reason": "reason",
      "to_quantity": "toQuantity",
    },
  },
};

/** Result of `wamn-wms:inventory/adjust@1.0.0`. */
export interface InventoryAdjustResult {
  /** `text` */
  readonly disposition: string;
  /** `uuid` */
  readonly inventoryId: Uuid;
  /** `text` */
  readonly lifecycle: string;
  /** `uuid` */
  readonly locationId: Uuid;
  /** `uuid` */
  readonly operationId: Uuid;
  /** `uuid` */
  readonly packagingId: Uuid;
  /** `uuid` */
  readonly productId: Uuid;
  /** `numeric` */
  readonly quantity: Numeric;
  /** `int32` */
  readonly rowVersion: number;
}

/** What `wamn-wms:inventory/adjust@1.0.0` calls its result members. */
export const INVENTORY_ADJUST_RESULT_FIELDS: FieldMap = {
  "disposition": "disposition",
  "inventory_id": "inventoryId",
  "lifecycle": "lifecycle",
  "location_id": "locationId",
  "operation_id": "operationId",
  "packaging_id": "packagingId",
  "product_id": "productId",
  "quantity": "quantity",
  "row_version": "rowVersion",
};

/**
 * Where the release publishes `wamn-wms:inventory/adjust@1.0.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const INVENTORY_ADJUST_ROUTE: OperationRoute = {
  operation: "wamn-wms:inventory/adjust@1.0.0",
  method: "POST",
  template: "/inventory/adjust",
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

/** Invoke `wamn-wms:inventory/adjust@1.0.0` through a transport the application supplies. */
export async function adjust(
  transport: Transport,
  items: readonly InventoryAdjustRequest[],
): Promise<Outcome<InventoryAdjustResult>> {
  return reviveOutcome<InventoryAdjustResult>(
    await transport.invoke({
      ...INVENTORY_ADJUST_ROUTE,
      items: items.map((item) => toWire(item, INVENTORY_ADJUST_REQUEST_FIELDS)),
    }),
    INVENTORY_ADJUST_RESULT_FIELDS,
  );
}

/** Input for `wamn-wms:inventory/aggregate@1.0.0`. */
export interface InventoryAggregateRequest {
}

/** What `wamn-wms:inventory/aggregate@1.0.0` calls its input members. */
export const INVENTORY_AGGREGATE_REQUEST_FIELDS: FieldMap = {};

/** One row of `wamn-wms:inventory/aggregate@1.0.0`. */
export interface InventoryAggregateRow {
  /** `text` */
  readonly disposition: "available" | "held";
  /** `uuid` */
  readonly locationId: Uuid;
  /** `int32` */
  readonly packagingCount: number;
  /** `uuid` */
  readonly productId: Uuid;
  /** `numeric` */
  readonly quantity: Numeric;
}

/** Result of `wamn-wms:inventory/aggregate@1.0.0`. */
export interface InventoryAggregateResult {
  /** Every row the release served. */
  readonly rows: readonly InventoryAggregateRow[];
}

/** What `wamn-wms:inventory/aggregate@1.0.0` calls its result members. */
export const INVENTORY_AGGREGATE_RESULT_FIELDS: FieldMap = {
  "rows": {
    member: "rows",
    fields: {
      "disposition": "disposition",
      "location_id": "locationId",
      "packaging_count": "packagingCount",
      "product_id": "productId",
      "quantity": "quantity",
    },
  },
};

/**
 * Where the release publishes `wamn-wms:inventory/aggregate@1.0.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const INVENTORY_AGGREGATE_ROUTE: OperationRoute = {
  operation: "wamn-wms:inventory/aggregate@1.0.0",
  method: "GET",
  template: "/inventory/aggregate",
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

/** Invoke `wamn-wms:inventory/aggregate@1.0.0` through a transport the application supplies. */
export async function aggregate(
  transport: Transport,
  items: readonly InventoryAggregateRequest[],
): Promise<Outcome<InventoryAggregateResult>> {
  return reviveOutcome<InventoryAggregateResult>(
    await transport.invoke({
      ...INVENTORY_AGGREGATE_ROUTE,
      items: items.map((item) => toWire(item, INVENTORY_AGGREGATE_REQUEST_FIELDS)),
    }),
    INVENTORY_AGGREGATE_RESULT_FIELDS,
  );
}

/** Input for `wamn-wms:inventory/get@1.0.0`. */
export interface InventoryGetRequest {
  /** `uuid` */
  id: Uuid;
}

/** What `wamn-wms:inventory/get@1.0.0` calls its input members. */
export const INVENTORY_GET_REQUEST_FIELDS: FieldMap = {
  "id": "id",
};

/** Result of `wamn-wms:inventory/get@1.0.0`. */
export interface InventoryGetResult {
  /** `timestamptz` */
  readonly createdAt: Timestamptz;
  /** `text` */
  readonly disposition: "available" | "held";
  /** `uuid` */
  readonly id: Uuid;
  /** `text` */
  readonly lifecycle: "closed" | "open";
  /** `uuid` */
  readonly locationId: Uuid;
  /** `uuid` */
  readonly packagingId: Uuid;
  /** `uuid` */
  readonly productId: Uuid;
  /** `numeric` */
  readonly quantity: Numeric;
  /** `int32` */
  readonly rowVersion: number;
}

/** What `wamn-wms:inventory/get@1.0.0` calls its result members. */
export const INVENTORY_GET_RESULT_FIELDS: FieldMap = {
  "created_at": "createdAt",
  "disposition": "disposition",
  "id": "id",
  "lifecycle": "lifecycle",
  "location_id": "locationId",
  "packaging_id": "packagingId",
  "product_id": "productId",
  "quantity": "quantity",
  "row_version": "rowVersion",
};

/**
 * Where the release publishes `wamn-wms:inventory/get@1.0.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const INVENTORY_GET_ROUTE: OperationRoute = {
  operation: "wamn-wms:inventory/get@1.0.0",
  method: "GET",
  template: "/inventory/get",
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

/** Invoke `wamn-wms:inventory/get@1.0.0` through a transport the application supplies. */
export async function get(
  transport: Transport,
  items: readonly InventoryGetRequest[],
): Promise<Outcome<InventoryGetResult>> {
  return reviveOutcome<InventoryGetResult>(
    await transport.invoke({
      ...INVENTORY_GET_ROUTE,
      items: items.map((item) => toWire(item, INVENTORY_GET_REQUEST_FIELDS)),
    }),
    INVENTORY_GET_RESULT_FIELDS,
  );
}

/** Input for `wamn-wms:inventory/merge@1.0.0`. */
export interface InventoryMergeRequest {
  /** `text` */
  requestId: string;
  /** `object` */
  value: InventoryMergeRequestValue;
}

export interface InventoryMergeRequestValue {
  /** `int32` */
  expectedFromRowVersion: number;
  /** `int32` */
  expectedToRowVersion: number;
  /** `uuid` */
  fromInventoryId: Uuid;
  /** `text` */
  idempotencyKey: string;
  /** `timestamptz` */
  occurredAt: Timestamptz;
  /** `uuid` */
  toInventoryId: Uuid;
}

/** What `wamn-wms:inventory/merge@1.0.0` calls its input members. */
export const INVENTORY_MERGE_REQUEST_FIELDS: FieldMap = {
  "request_id": "requestId",
  "value": {
    member: "value",
    fields: {
      "expected_from_row_version": "expectedFromRowVersion",
      "expected_to_row_version": "expectedToRowVersion",
      "from_inventory_id": "fromInventoryId",
      "idempotency_key": "idempotencyKey",
      "occurred_at": "occurredAt",
      "to_inventory_id": "toInventoryId",
    },
  },
};

/** Result of `wamn-wms:inventory/merge@1.0.0`. */
export interface InventoryMergeResult {
  /** `text` */
  readonly disposition: string;
  /** `uuid` */
  readonly fromInventoryId: Uuid;
  /** `uuid` */
  readonly inventoryId: Uuid;
  /** `text` */
  readonly lifecycle: string;
  /** `uuid` */
  readonly locationId: Uuid;
  /** `uuid` */
  readonly operationId: Uuid;
  /** `uuid` */
  readonly packagingId: Uuid;
  /** `uuid` */
  readonly productId: Uuid;
  /** `numeric` */
  readonly quantity: Numeric;
  /** `int32` */
  readonly rowVersion: number;
}

/** What `wamn-wms:inventory/merge@1.0.0` calls its result members. */
export const INVENTORY_MERGE_RESULT_FIELDS: FieldMap = {
  "disposition": "disposition",
  "from_inventory_id": "fromInventoryId",
  "inventory_id": "inventoryId",
  "lifecycle": "lifecycle",
  "location_id": "locationId",
  "operation_id": "operationId",
  "packaging_id": "packagingId",
  "product_id": "productId",
  "quantity": "quantity",
  "row_version": "rowVersion",
};

/**
 * Where the release publishes `wamn-wms:inventory/merge@1.0.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const INVENTORY_MERGE_ROUTE: OperationRoute = {
  operation: "wamn-wms:inventory/merge@1.0.0",
  method: "POST",
  template: "/inventory/merge",
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

/** Invoke `wamn-wms:inventory/merge@1.0.0` through a transport the application supplies. */
export async function merge(
  transport: Transport,
  items: readonly InventoryMergeRequest[],
): Promise<Outcome<InventoryMergeResult>> {
  return reviveOutcome<InventoryMergeResult>(
    await transport.invoke({
      ...INVENTORY_MERGE_ROUTE,
      items: items.map((item) => toWire(item, INVENTORY_MERGE_REQUEST_FIELDS)),
    }),
    INVENTORY_MERGE_RESULT_FIELDS,
  );
}

/** Input for `wamn-wms:inventory/move@1.0.0`. */
export interface InventoryMoveRequest {
  /** `text` */
  requestId: string;
  /** `object` */
  value: InventoryMoveRequestValue;
}

export interface InventoryMoveRequestValue {
  /** `int32` */
  expectedRowVersion: number;
  /** `text` */
  idempotencyKey: string;
  /** `uuid` */
  inventoryId: Uuid;
  /** `timestamptz` */
  occurredAt: Timestamptz;
  /** `uuid` */
  toLocationId: Uuid;
  /** `uuid` */
  toPackagingId: Uuid;
}

/** What `wamn-wms:inventory/move@1.0.0` calls its input members. */
export const INVENTORY_MOVE_REQUEST_FIELDS: FieldMap = {
  "request_id": "requestId",
  "value": {
    member: "value",
    fields: {
      "expected_row_version": "expectedRowVersion",
      "idempotency_key": "idempotencyKey",
      "inventory_id": "inventoryId",
      "occurred_at": "occurredAt",
      "to_location_id": "toLocationId",
      "to_packaging_id": "toPackagingId",
    },
  },
};

/** Result of `wamn-wms:inventory/move@1.0.0`. */
export interface InventoryMoveResult {
  /** `text` */
  readonly disposition: string;
  /** `uuid` */
  readonly inventoryId: Uuid;
  /** `text` */
  readonly lifecycle: string;
  /** `uuid` */
  readonly locationId: Uuid;
  /** `uuid` */
  readonly operationId: Uuid;
  /** `uuid` */
  readonly packagingId: Uuid;
  /** `uuid` */
  readonly productId: Uuid;
  /** `numeric` */
  readonly quantity: Numeric;
  /** `int32` */
  readonly rowVersion: number;
}

/** What `wamn-wms:inventory/move@1.0.0` calls its result members. */
export const INVENTORY_MOVE_RESULT_FIELDS: FieldMap = {
  "disposition": "disposition",
  "inventory_id": "inventoryId",
  "lifecycle": "lifecycle",
  "location_id": "locationId",
  "operation_id": "operationId",
  "packaging_id": "packagingId",
  "product_id": "productId",
  "quantity": "quantity",
  "row_version": "rowVersion",
};

/**
 * Where the release publishes `wamn-wms:inventory/move@1.0.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const INVENTORY_MOVE_ROUTE: OperationRoute = {
  operation: "wamn-wms:inventory/move@1.0.0",
  method: "POST",
  template: "/inventory/move",
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

/** Invoke `wamn-wms:inventory/move@1.0.0` through a transport the application supplies. */
export async function move(
  transport: Transport,
  items: readonly InventoryMoveRequest[],
): Promise<Outcome<InventoryMoveResult>> {
  return reviveOutcome<InventoryMoveResult>(
    await transport.invoke({
      ...INVENTORY_MOVE_ROUTE,
      items: items.map((item) => toWire(item, INVENTORY_MOVE_REQUEST_FIELDS)),
    }),
    INVENTORY_MOVE_RESULT_FIELDS,
  );
}

/** Input for `wamn-wms:inventory/query@1.0.0`. */
export interface InventoryQueryRequest {
  /** `text`, omittable */
  cursor?: string;
  /** `int32`, omittable */
  limit?: number;
}

/** What `wamn-wms:inventory/query@1.0.0` calls its input members. */
export const INVENTORY_QUERY_REQUEST_FIELDS: FieldMap = {
  "cursor": "cursor",
  "limit": "limit",
};

/** One row of `wamn-wms:inventory/query@1.0.0`. */
export interface InventoryQueryRow {
  /** `timestamptz` */
  readonly createdAt: Timestamptz;
  /** `text` */
  readonly disposition: "available" | "held";
  /** `uuid` */
  readonly id: Uuid;
  /** `text` */
  readonly lifecycle: "closed" | "open";
  /** `uuid` */
  readonly locationId: Uuid;
  /** `uuid` */
  readonly packagingId: Uuid;
  /** `uuid` */
  readonly productId: Uuid;
  /** `numeric` */
  readonly quantity: Numeric;
  /** `int32` */
  readonly rowVersion: number;
}

/** Result of `wamn-wms:inventory/query@1.0.0`. */
export interface InventoryQueryResult {
  /** The rows this page carries. */
  readonly item: readonly InventoryQueryRow[];
  /** The next page's cursor, or null at the last page. */
  readonly nextCursor: string | null;
}

/** What `wamn-wms:inventory/query@1.0.0` calls its result members. */
export const INVENTORY_QUERY_RESULT_FIELDS: FieldMap = {
  "item": {
    member: "item",
    fields: {
      "created_at": "createdAt",
      "disposition": "disposition",
      "id": "id",
      "lifecycle": "lifecycle",
      "location_id": "locationId",
      "packaging_id": "packagingId",
      "product_id": "productId",
      "quantity": "quantity",
      "row_version": "rowVersion",
    },
  },
  "next_cursor": "nextCursor",
};

/**
 * Where the release publishes `wamn-wms:inventory/query@1.0.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const INVENTORY_QUERY_ROUTE: OperationRoute = {
  operation: "wamn-wms:inventory/query@1.0.0",
  method: "GET",
  template: "/inventory/query",
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

/** Invoke `wamn-wms:inventory/query@1.0.0` through a transport the application supplies. */
export async function query(
  transport: Transport,
  items: readonly InventoryQueryRequest[],
): Promise<Outcome<InventoryQueryResult>> {
  return reviveOutcome<InventoryQueryResult>(
    await transport.invoke({
      ...INVENTORY_QUERY_ROUTE,
      items: items.map((item) => toWire(item, INVENTORY_QUERY_REQUEST_FIELDS)),
    }),
    INVENTORY_QUERY_RESULT_FIELDS,
  );
}

/** Input for `wamn-wms:inventory/split@1.0.0`. */
export interface InventorySplitRequest {
  /** `text` */
  requestId: string;
  /** `object` */
  value: InventorySplitRequestValue;
}

export interface InventorySplitRequestValue {
  /** `int32` */
  expectedRowVersion: number;
  /** `uuid` */
  fromInventoryId: Uuid;
  /** `text` */
  idempotencyKey: string;
  /** `timestamptz` */
  occurredAt: Timestamptz;
  /** `numeric` */
  quantity: Numeric;
  /** `uuid` */
  toLocationId: Uuid;
  /** `uuid` */
  toPackagingId: Uuid;
}

/** What `wamn-wms:inventory/split@1.0.0` calls its input members. */
export const INVENTORY_SPLIT_REQUEST_FIELDS: FieldMap = {
  "request_id": "requestId",
  "value": {
    member: "value",
    fields: {
      "expected_row_version": "expectedRowVersion",
      "from_inventory_id": "fromInventoryId",
      "idempotency_key": "idempotencyKey",
      "occurred_at": "occurredAt",
      "quantity": "quantity",
      "to_location_id": "toLocationId",
      "to_packaging_id": "toPackagingId",
    },
  },
};

/** Result of `wamn-wms:inventory/split@1.0.0`. */
export interface InventorySplitResult {
  /** `text` */
  readonly disposition: string;
  /** `uuid` */
  readonly inventoryId: Uuid;
  /** `text` */
  readonly lifecycle: string;
  /** `uuid` */
  readonly locationId: Uuid;
  /** `uuid` */
  readonly newInventoryId: Uuid;
  /** `uuid` */
  readonly operationId: Uuid;
  /** `uuid` */
  readonly packagingId: Uuid;
  /** `uuid` */
  readonly productId: Uuid;
  /** `numeric` */
  readonly quantity: Numeric;
  /** `int32` */
  readonly rowVersion: number;
}

/** What `wamn-wms:inventory/split@1.0.0` calls its result members. */
export const INVENTORY_SPLIT_RESULT_FIELDS: FieldMap = {
  "disposition": "disposition",
  "inventory_id": "inventoryId",
  "lifecycle": "lifecycle",
  "location_id": "locationId",
  "new_inventory_id": "newInventoryId",
  "operation_id": "operationId",
  "packaging_id": "packagingId",
  "product_id": "productId",
  "quantity": "quantity",
  "row_version": "rowVersion",
};

/**
 * Where the release publishes `wamn-wms:inventory/split@1.0.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const INVENTORY_SPLIT_ROUTE: OperationRoute = {
  operation: "wamn-wms:inventory/split@1.0.0",
  method: "POST",
  template: "/inventory/split",
  freshOnly: false,
  contract: {
    resultClass: "one",
    partialSchema: null,
    errors: [
      { literal: "concurrency_conflict", required: ["expected_row_version", "observed_row_version"], sources: ["transaction_invariant"] },
      { literal: "idempotency_conflict", required: ["field"], sources: ["same_key_different_canonical_command"] },
      { literal: "insufficient_quantity", required: ["field"], sources: ["transaction_invariant"] },
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

/** Invoke `wamn-wms:inventory/split@1.0.0` through a transport the application supplies. */
export async function split(
  transport: Transport,
  items: readonly InventorySplitRequest[],
): Promise<Outcome<InventorySplitResult>> {
  return reviveOutcome<InventorySplitResult>(
    await transport.invoke({
      ...INVENTORY_SPLIT_ROUTE,
      items: items.map((item) => toWire(item, INVENTORY_SPLIT_REQUEST_FIELDS)),
    }),
    INVENTORY_SPLIT_RESULT_FIELDS,
  );
}
