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
  /** `timestamptz` */
  occurredAt: Timestamptz;
  /** `uuid` */
  palletId: Uuid;
  /** `uuid` */
  productId: Uuid;
  /** `numeric` */
  quantity: Numeric;
  /** `text` */
  reasonCode: string;
  /** `text` */
  status: "available" | "held";
}

/** What `wamn-wms:inventory/adjust@1.0.0` calls its input members. */
export const INVENTORY_ADJUST_REQUEST_FIELDS: FieldMap = {
  "request_id": "requestId",
  "value": {
    member: "value",
    fields: {
      "expected_row_version": "expectedRowVersion",
      "idempotency_key": "idempotencyKey",
      "occurred_at": "occurredAt",
      "pallet_id": "palletId",
      "product_id": "productId",
      "quantity": "quantity",
      "reason_code": "reasonCode",
      "status": "status",
    },
  },
};

/** Result of `wamn-wms:inventory/adjust@1.0.0`. */
export interface InventoryAdjustResult {
  /** `numeric` */
  readonly adjustedQuantity: Numeric;
  /** `uuid` */
  readonly movementId: Uuid;
  /** `uuid` */
  readonly palletId: Uuid;
  /** `text` */
  readonly palletStatus: "available" | "consumed" | "held";
  /** `int32` */
  readonly rowVersion: number;
}

/** What `wamn-wms:inventory/adjust@1.0.0` calls its result members. */
export const INVENTORY_ADJUST_RESULT_FIELDS: FieldMap = {
  "adjusted_quantity": "adjustedQuantity",
  "movement_id": "movementId",
  "pallet_id": "palletId",
  "pallet_status": "palletStatus",
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
      { literal: "pallet_not_found", required: ["field", "id"], sources: ["transaction_invariant"] },
      { literal: "permission_denied", required: ["operation"], sources: ["permission_denied"] },
      { literal: "quantity_not_found", required: ["field", "id"], sources: ["transaction_invariant"] },
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
  /** `uuid` */
  readonly locationId: Uuid;
  /** `int32` */
  readonly palletCount: number;
  /** `uuid` */
  readonly productId: Uuid;
  /** `numeric` */
  readonly quantity: Numeric;
  /** `text` */
  readonly status: "available" | "held";
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
      "location_id": "locationId",
      "pallet_count": "palletCount",
      "product_id": "productId",
      "quantity": "quantity",
      "status": "status",
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

/** Input for `wamn-wms:inventory/merge@1.0.0`. */
export interface InventoryMergeRequest {
  /** `text` */
  requestId: string;
  /** `object` */
  value: InventoryMergeRequestValue;
}

export interface InventoryMergeRequestValue {
  /** `int32` */
  expectedRowVersion: number;
  /** `text` */
  idempotencyKey: string;
  /** `timestamptz` */
  occurredAt: Timestamptz;
  /** `uuid` */
  sourcePalletId: Uuid;
  /** `uuid` */
  targetPalletId: Uuid;
}

/** What `wamn-wms:inventory/merge@1.0.0` calls its input members. */
export const INVENTORY_MERGE_REQUEST_FIELDS: FieldMap = {
  "request_id": "requestId",
  "value": {
    member: "value",
    fields: {
      "expected_row_version": "expectedRowVersion",
      "idempotency_key": "idempotencyKey",
      "occurred_at": "occurredAt",
      "source_pallet_id": "sourcePalletId",
      "target_pallet_id": "targetPalletId",
    },
  },
};

/** Result of `wamn-wms:inventory/merge@1.0.0`. */
export interface InventoryMergeResult {
  /** `uuid` */
  readonly movementId: Uuid;
  /** `int32` */
  readonly rowVersion: number;
  /** `uuid` */
  readonly sourcePalletId: Uuid;
  /** `uuid` */
  readonly targetPalletId: Uuid;
  /** `text` */
  readonly targetStatus: "available" | "consumed" | "held";
}

/** What `wamn-wms:inventory/merge@1.0.0` calls its result members. */
export const INVENTORY_MERGE_RESULT_FIELDS: FieldMap = {
  "movement_id": "movementId",
  "row_version": "rowVersion",
  "source_pallet_id": "sourcePalletId",
  "target_pallet_id": "targetPalletId",
  "target_status": "targetStatus",
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
      { literal: "pallet_not_found", required: ["field", "id"], sources: ["transaction_invariant"] },
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
  /** `timestamptz` */
  occurredAt: Timestamptz;
  /** `uuid` */
  palletId: Uuid;
  /** `uuid` */
  toLocationId: Uuid;
}

/** What `wamn-wms:inventory/move@1.0.0` calls its input members. */
export const INVENTORY_MOVE_REQUEST_FIELDS: FieldMap = {
  "request_id": "requestId",
  "value": {
    member: "value",
    fields: {
      "expected_row_version": "expectedRowVersion",
      "idempotency_key": "idempotencyKey",
      "occurred_at": "occurredAt",
      "pallet_id": "palletId",
      "to_location_id": "toLocationId",
    },
  },
};

/** Result of `wamn-wms:inventory/move@1.0.0`. */
export interface InventoryMoveResult {
  /** `uuid` */
  readonly locationId: Uuid;
  /** `uuid` */
  readonly movementId: Uuid;
  /** `uuid` */
  readonly palletId: Uuid;
  /** `text` */
  readonly palletStatus: "available" | "consumed" | "held";
  /** `int32` */
  readonly rowVersion: number;
}

/** What `wamn-wms:inventory/move@1.0.0` calls its result members. */
export const INVENTORY_MOVE_RESULT_FIELDS: FieldMap = {
  "location_id": "locationId",
  "movement_id": "movementId",
  "pallet_id": "palletId",
  "pallet_status": "palletStatus",
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
      { literal: "location_not_found", required: ["field", "id"], sources: ["transaction_invariant"] },
      { literal: "pallet_not_found", required: ["field", "id"], sources: ["transaction_invariant"] },
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
  /** `text` */
  idempotencyKey: string;
  /** `text` */
  newPalletCode: string;
  /** `timestamptz` */
  occurredAt: Timestamptz;
  /** `uuid` */
  productId: Uuid;
  /** `numeric` */
  quantity: Numeric;
  /** `uuid` */
  sourcePalletId: Uuid;
  /** `text` */
  status: "available" | "held";
  /** `uuid` */
  toLocationId: Uuid;
}

/** What `wamn-wms:inventory/split@1.0.0` calls its input members. */
export const INVENTORY_SPLIT_REQUEST_FIELDS: FieldMap = {
  "request_id": "requestId",
  "value": {
    member: "value",
    fields: {
      "expected_row_version": "expectedRowVersion",
      "idempotency_key": "idempotencyKey",
      "new_pallet_code": "newPalletCode",
      "occurred_at": "occurredAt",
      "product_id": "productId",
      "quantity": "quantity",
      "source_pallet_id": "sourcePalletId",
      "status": "status",
      "to_location_id": "toLocationId",
    },
  },
};

/** Result of `wamn-wms:inventory/split@1.0.0`. */
export interface InventorySplitResult {
  /** `uuid` */
  readonly movementId: Uuid;
  /** `uuid` */
  readonly newPalletId: Uuid;
  /** `int32` */
  readonly rowVersion: number;
  /** `uuid` */
  readonly sourcePalletId: Uuid;
  /** `text` */
  readonly sourceStatus: "available" | "consumed" | "held";
}

/** What `wamn-wms:inventory/split@1.0.0` calls its result members. */
export const INVENTORY_SPLIT_RESULT_FIELDS: FieldMap = {
  "movement_id": "movementId",
  "new_pallet_id": "newPalletId",
  "row_version": "rowVersion",
  "source_pallet_id": "sourcePalletId",
  "source_status": "sourceStatus",
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
      { literal: "location_not_found", required: ["field", "id"], sources: ["transaction_invariant"] },
      { literal: "pallet_not_found", required: ["field", "id"], sources: ["transaction_invariant"] },
      { literal: "permission_denied", required: ["operation"], sources: ["permission_denied"] },
      { literal: "quantity_not_found", required: ["field", "id"], sources: ["transaction_invariant"] },
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
