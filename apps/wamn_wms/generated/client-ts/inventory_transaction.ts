// @generated from the client-contract IR; do not edit.
//
// `inventory_transaction` operations of package `wamn_wms`.

import type { FieldMap, JsonValue, Numeric, OperationRoute, Outcome, Timestamptz, Transport, Uuid } from "@wamn/web-runtime";
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
  /** `uuid` */
  readonly createdBy: Uuid;
  /** `uuid` */
  readonly fromPackagingId: Uuid | null;
  /** `text` */
  readonly fromStatus: string | null;
  /** `uuid` */
  readonly id: Uuid;
  /** `timestamptz` */
  readonly occurredAt: Timestamptz;
  /** `uuid` */
  readonly productId: Uuid;
  /** `numeric` */
  readonly quantity: Numeric;
  /** `text` */
  readonly reasonCode: string | null;
  /** `uuid` */
  readonly toPackagingId: Uuid | null;
  /** `text` */
  readonly toStatus: string | null;
}

/** What `wamn-wms:inventory-transaction/get@1.0.0` calls its result members. */
export const INVENTORY_TRANSACTION_GET_RESULT_FIELDS: FieldMap = {
  "created_at": "createdAt",
  "created_by": "createdBy",
  "from_packaging_id": "fromPackagingId",
  "from_status": "fromStatus",
  "id": "id",
  "occurred_at": "occurredAt",
  "product_id": "productId",
  "quantity": "quantity",
  "reason_code": "reasonCode",
  "to_packaging_id": "toPackagingId",
  "to_status": "toStatus",
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
  /** `keyset`, omittable */
  pagination?: JsonValue;
}

/** What `wamn-wms:inventory-transaction/query@1.0.0` calls its input members. */
export const INVENTORY_TRANSACTION_QUERY_REQUEST_FIELDS: FieldMap = {
  "cursor": "cursor",
  "limit": "limit",
  "pagination": "pagination",
};

/** One row of `wamn-wms:inventory-transaction/query@1.0.0`. */
export interface InventoryTransactionQueryRow {
  /** `timestamptz` */
  readonly createdAt: Timestamptz;
  /** `uuid` */
  readonly createdBy: Uuid;
  /** `uuid` */
  readonly fromPackagingId: Uuid | null;
  /** `text` */
  readonly fromStatus: string | null;
  /** `uuid` */
  readonly id: Uuid;
  /** `timestamptz` */
  readonly occurredAt: Timestamptz;
  /** `uuid` */
  readonly productId: Uuid;
  /** `numeric` */
  readonly quantity: Numeric;
  /** `text` */
  readonly reasonCode: string | null;
  /** `uuid` */
  readonly toPackagingId: Uuid | null;
  /** `text` */
  readonly toStatus: string | null;
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
      "created_by": "createdBy",
      "from_packaging_id": "fromPackagingId",
      "from_status": "fromStatus",
      "id": "id",
      "occurred_at": "occurredAt",
      "product_id": "productId",
      "quantity": "quantity",
      "reason_code": "reasonCode",
      "to_packaging_id": "toPackagingId",
      "to_status": "toStatus",
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
