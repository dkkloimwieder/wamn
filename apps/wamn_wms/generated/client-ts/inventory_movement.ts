// @generated from the client-contract IR; do not edit.
//
// `inventory_movement` operations of package `wamn_wms`.

import type { FieldMap, Numeric, OperationRoute, Outcome, Timestamptz, Transport, Uuid } from "@wamn/web-runtime";
import { reviveOutcome, toWire } from "@wamn/web-runtime";

/** Input for `wamn-wms:inventory-movement/get@1.0.0`. */
export interface InventoryMovementGetRequest {
  /** `uuid` */
  id: Uuid;
}

/** What `wamn-wms:inventory-movement/get@1.0.0` calls its input members. */
export const INVENTORY_MOVEMENT_GET_REQUEST_FIELDS: FieldMap = {
  "id": "id",
};

/** Result of `wamn-wms:inventory-movement/get@1.0.0`. */
export interface InventoryMovementGetResult {
  /** `timestamptz` */
  readonly createdAt: Timestamptz;
  /** `uuid` */
  readonly createdBy: Uuid;
  /** `uuid` */
  readonly fromLocationId: Uuid | null;
  /** `uuid` */
  readonly id: Uuid;
  /** `text` */
  readonly idempotencyKey: string;
  /** `text` */
  readonly kind: string;
  /** `timestamptz` */
  readonly occurredAt: Timestamptz;
  /** `uuid` */
  readonly palletId: Uuid;
  /** `uuid` */
  readonly productId: Uuid;
  /** `numeric` */
  readonly quantity: Numeric;
  /** `text` */
  readonly reasonCode: string | null;
  /** `uuid` */
  readonly toLocationId: Uuid | null;
}

/** What `wamn-wms:inventory-movement/get@1.0.0` calls its result members. */
export const INVENTORY_MOVEMENT_GET_RESULT_FIELDS: FieldMap = {
  "created_at": "createdAt",
  "created_by": "createdBy",
  "from_location_id": "fromLocationId",
  "id": "id",
  "idempotency_key": "idempotencyKey",
  "kind": "kind",
  "occurred_at": "occurredAt",
  "pallet_id": "palletId",
  "product_id": "productId",
  "quantity": "quantity",
  "reason_code": "reasonCode",
  "to_location_id": "toLocationId",
};

/**
 * Where the release publishes `wamn-wms:inventory-movement/get@1.0.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const INVENTORY_MOVEMENT_GET_ROUTE: OperationRoute = {
  operation: "wamn-wms:inventory-movement/get@1.0.0",
  method: "GET",
  template: "/inventory_movement/get",
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
    kind: "get",
    transaction: "implicit",
  },
};

/** Invoke `wamn-wms:inventory-movement/get@1.0.0` through a transport the application supplies. */
export async function get(
  transport: Transport,
  items: readonly InventoryMovementGetRequest[],
): Promise<Outcome<InventoryMovementGetResult>> {
  return reviveOutcome<InventoryMovementGetResult>(
    await transport.invoke({
      ...INVENTORY_MOVEMENT_GET_ROUTE,
      items: items.map((item) => toWire(item, INVENTORY_MOVEMENT_GET_REQUEST_FIELDS)),
    }),
    INVENTORY_MOVEMENT_GET_RESULT_FIELDS,
  );
}

/** Input for `wamn-wms:inventory-movement/query@1.0.0`. */
export interface InventoryMovementQueryRequest {
  /** `text`, omittable */
  cursor?: string;
  /** `int32`, omittable */
  limit?: number;
}

/** What `wamn-wms:inventory-movement/query@1.0.0` calls its input members. */
export const INVENTORY_MOVEMENT_QUERY_REQUEST_FIELDS: FieldMap = {
  "cursor": "cursor",
  "limit": "limit",
};

/** One row of `wamn-wms:inventory-movement/query@1.0.0`. */
export interface InventoryMovementQueryRow {
  /** `timestamptz` */
  readonly createdAt: Timestamptz;
  /** `uuid` */
  readonly createdBy: Uuid;
  /** `uuid` */
  readonly fromLocationId: Uuid | null;
  /** `uuid` */
  readonly id: Uuid;
  /** `text` */
  readonly idempotencyKey: string;
  /** `text` */
  readonly kind: string;
  /** `timestamptz` */
  readonly occurredAt: Timestamptz;
  /** `uuid` */
  readonly palletId: Uuid;
  /** `uuid` */
  readonly productId: Uuid;
  /** `numeric` */
  readonly quantity: Numeric;
  /** `text` */
  readonly reasonCode: string | null;
  /** `uuid` */
  readonly toLocationId: Uuid | null;
}

/** Result of `wamn-wms:inventory-movement/query@1.0.0`. */
export interface InventoryMovementQueryResult {
  /** The rows this page carries. */
  readonly item: readonly InventoryMovementQueryRow[];
  /** The next page's cursor, or null at the last page. */
  readonly nextCursor: string | null;
}

/** What `wamn-wms:inventory-movement/query@1.0.0` calls its result members. */
export const INVENTORY_MOVEMENT_QUERY_RESULT_FIELDS: FieldMap = {
  "item": {
    member: "item",
    fields: {
      "created_at": "createdAt",
      "created_by": "createdBy",
      "from_location_id": "fromLocationId",
      "id": "id",
      "idempotency_key": "idempotencyKey",
      "kind": "kind",
      "occurred_at": "occurredAt",
      "pallet_id": "palletId",
      "product_id": "productId",
      "quantity": "quantity",
      "reason_code": "reasonCode",
      "to_location_id": "toLocationId",
    },
  },
  "next_cursor": "nextCursor",
};

/**
 * Where the release publishes `wamn-wms:inventory-movement/query@1.0.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const INVENTORY_MOVEMENT_QUERY_ROUTE: OperationRoute = {
  operation: "wamn-wms:inventory-movement/query@1.0.0",
  method: "GET",
  template: "/inventory_movement/query",
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
    kind: "query",
    transaction: "implicit",
  },
};

/** Invoke `wamn-wms:inventory-movement/query@1.0.0` through a transport the application supplies. */
export async function query(
  transport: Transport,
  items: readonly InventoryMovementQueryRequest[],
): Promise<Outcome<InventoryMovementQueryResult>> {
  return reviveOutcome<InventoryMovementQueryResult>(
    await transport.invoke({
      ...INVENTORY_MOVEMENT_QUERY_ROUTE,
      items: items.map((item) => toWire(item, INVENTORY_MOVEMENT_QUERY_REQUEST_FIELDS)),
    }),
    INVENTORY_MOVEMENT_QUERY_RESULT_FIELDS,
  );
}
