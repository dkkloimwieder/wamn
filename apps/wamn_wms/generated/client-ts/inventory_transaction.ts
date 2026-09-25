// @generated from the client-contract IR; do not edit.
//
// `inventory_transaction` operations of package `wamn_wms`.

import type { FieldMap, Numeric, OperationRoute, Outcome, Timestamptz, Transport, Uuid } from "@wamn/web-runtime";
import { reviveOutcome, toWire } from "@wamn/web-runtime";

/** Input for `wamn-wms:inventory-transaction/get@1.0.0`. */
export interface InventoryTransactionGetRequest {
  /** `uuid` */
  id: Uuid;
}

/** What `wamn-wms:inventory-transaction/get@1.0.0` calls its input members. */
export const INVENTORY_TRANSACTION_GET_REQUEST_FIELDS: FieldMap = {
  "id": "id",
};

/** Result of `wamn-wms:inventory-transaction/get@1.0.0`. */
export interface InventoryTransactionGetResult {
  /** `timestamptz` */
  readonly createdAt: Timestamptz;
  /** `text` */
  readonly fromDisposition: string | null;
  /** `uuid` */
  readonly fromInventoryId: Uuid;
  /** `text` */
  readonly fromLifecycle: string | null;
  /** `uuid` */
  readonly fromLocationId: Uuid | null;
  /** `uuid` */
  readonly fromPackagingId: Uuid | null;
  /** `uuid` */
  readonly fromProductId: Uuid | null;
  /** `numeric` */
  readonly fromQuantity: Numeric;
  /** `uuid` */
  readonly id: Uuid;
  /** `uuid` */
  readonly inventoryId: Uuid;
  /** `timestamptz` */
  readonly occurredAt: Timestamptz;
  /** `uuid` */
  readonly operationId: Uuid;
  /** `text` */
  readonly reason: string | null;
  /** `text` */
  readonly toDisposition: string;
  /** `uuid` */
  readonly toInventoryId: Uuid;
  /** `text` */
  readonly toLifecycle: string;
  /** `uuid` */
  readonly toLocationId: Uuid;
  /** `uuid` */
  readonly toPackagingId: Uuid;
  /** `uuid` */
  readonly toProductId: Uuid;
  /** `numeric` */
  readonly toQuantity: Numeric;
  /** `text` */
  readonly type: string;
}

/** What `wamn-wms:inventory-transaction/get@1.0.0` calls its result members. */
export const INVENTORY_TRANSACTION_GET_RESULT_FIELDS: FieldMap = {
  "created_at": "createdAt",
  "from_disposition": "fromDisposition",
  "from_inventory_id": "fromInventoryId",
  "from_lifecycle": "fromLifecycle",
  "from_location_id": "fromLocationId",
  "from_packaging_id": "fromPackagingId",
  "from_product_id": "fromProductId",
  "from_quantity": "fromQuantity",
  "id": "id",
  "inventory_id": "inventoryId",
  "occurred_at": "occurredAt",
  "operation_id": "operationId",
  "reason": "reason",
  "to_disposition": "toDisposition",
  "to_inventory_id": "toInventoryId",
  "to_lifecycle": "toLifecycle",
  "to_location_id": "toLocationId",
  "to_packaging_id": "toPackagingId",
  "to_product_id": "toProductId",
  "to_quantity": "toQuantity",
  "type": "type",
};

/**
 * Where the release publishes `wamn-wms:inventory-transaction/get@1.0.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const INVENTORY_TRANSACTION_GET_ROUTE: OperationRoute = {
  operation: "wamn-wms:inventory-transaction/get@1.0.0",
  method: "GET",
  template: "/inventory_transaction/get",
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

/** Invoke `wamn-wms:inventory-transaction/get@1.0.0` through a transport the application supplies. */
export async function get(
  transport: Transport,
  items: readonly InventoryTransactionGetRequest[],
): Promise<Outcome<InventoryTransactionGetResult>> {
  return reviveOutcome<InventoryTransactionGetResult>(
    await transport.invoke({
      ...INVENTORY_TRANSACTION_GET_ROUTE,
      items: items.map((item) => toWire(item, INVENTORY_TRANSACTION_GET_REQUEST_FIELDS)),
    }),
    INVENTORY_TRANSACTION_GET_RESULT_FIELDS,
  );
}

/** Input for `wamn-wms:inventory-transaction/query@1.0.0`. */
export interface InventoryTransactionQueryRequest {
  /** `text`, omittable */
  cursor?: string;
  /** `int32`, omittable */
  limit?: number;
}

/** What `wamn-wms:inventory-transaction/query@1.0.0` calls its input members. */
export const INVENTORY_TRANSACTION_QUERY_REQUEST_FIELDS: FieldMap = {
  "cursor": "cursor",
  "limit": "limit",
};

/** One row of `wamn-wms:inventory-transaction/query@1.0.0`. */
export interface InventoryTransactionQueryRow {
  /** `timestamptz` */
  readonly createdAt: Timestamptz;
  /** `text` */
  readonly fromDisposition: string | null;
  /** `uuid` */
  readonly fromInventoryId: Uuid;
  /** `text` */
  readonly fromLifecycle: string | null;
  /** `uuid` */
  readonly fromLocationId: Uuid | null;
  /** `uuid` */
  readonly fromPackagingId: Uuid | null;
  /** `uuid` */
  readonly fromProductId: Uuid | null;
  /** `numeric` */
  readonly fromQuantity: Numeric;
  /** `uuid` */
  readonly id: Uuid;
  /** `uuid` */
  readonly inventoryId: Uuid;
  /** `timestamptz` */
  readonly occurredAt: Timestamptz;
  /** `uuid` */
  readonly operationId: Uuid;
  /** `text` */
  readonly reason: string | null;
  /** `text` */
  readonly toDisposition: string;
  /** `uuid` */
  readonly toInventoryId: Uuid;
  /** `text` */
  readonly toLifecycle: string;
  /** `uuid` */
  readonly toLocationId: Uuid;
  /** `uuid` */
  readonly toPackagingId: Uuid;
  /** `uuid` */
  readonly toProductId: Uuid;
  /** `numeric` */
  readonly toQuantity: Numeric;
  /** `text` */
  readonly type: string;
}

/** Result of `wamn-wms:inventory-transaction/query@1.0.0`. */
export interface InventoryTransactionQueryResult {
  /** The rows this page carries. */
  readonly item: readonly InventoryTransactionQueryRow[];
  /** The next page's cursor, or null at the last page. */
  readonly nextCursor: string | null;
}

/** What `wamn-wms:inventory-transaction/query@1.0.0` calls its result members. */
export const INVENTORY_TRANSACTION_QUERY_RESULT_FIELDS: FieldMap = {
  "item": {
    member: "item",
    fields: {
      "created_at": "createdAt",
      "from_disposition": "fromDisposition",
      "from_inventory_id": "fromInventoryId",
      "from_lifecycle": "fromLifecycle",
      "from_location_id": "fromLocationId",
      "from_packaging_id": "fromPackagingId",
      "from_product_id": "fromProductId",
      "from_quantity": "fromQuantity",
      "id": "id",
      "inventory_id": "inventoryId",
      "occurred_at": "occurredAt",
      "operation_id": "operationId",
      "reason": "reason",
      "to_disposition": "toDisposition",
      "to_inventory_id": "toInventoryId",
      "to_lifecycle": "toLifecycle",
      "to_location_id": "toLocationId",
      "to_packaging_id": "toPackagingId",
      "to_product_id": "toProductId",
      "to_quantity": "toQuantity",
      "type": "type",
    },
  },
  "next_cursor": "nextCursor",
};

/**
 * Where the release publishes `wamn-wms:inventory-transaction/query@1.0.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const INVENTORY_TRANSACTION_QUERY_ROUTE: OperationRoute = {
  operation: "wamn-wms:inventory-transaction/query@1.0.0",
  method: "GET",
  template: "/inventory_transaction/query",
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

/** Invoke `wamn-wms:inventory-transaction/query@1.0.0` through a transport the application supplies. */
export async function query(
  transport: Transport,
  items: readonly InventoryTransactionQueryRequest[],
): Promise<Outcome<InventoryTransactionQueryResult>> {
  return reviveOutcome<InventoryTransactionQueryResult>(
    await transport.invoke({
      ...INVENTORY_TRANSACTION_QUERY_ROUTE,
      items: items.map((item) => toWire(item, INVENTORY_TRANSACTION_QUERY_REQUEST_FIELDS)),
    }),
    INVENTORY_TRANSACTION_QUERY_RESULT_FIELDS,
  );
}
