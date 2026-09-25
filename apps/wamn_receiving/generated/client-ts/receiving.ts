// @generated from the client-contract IR; do not edit.
//
// `receiving` operations of package `wamn_receiving`.

import type { FieldMap, Numeric, OperationRoute, Outcome, Timestamptz, Transport, Uuid } from "@wamn/web-runtime";
import { reviveOutcome, toWire } from "@wamn/web-runtime";

/** Input for `wamn-receiving:receiving/load-purchase-order-history@1.0.0`. */
export interface ReceivingLoadPurchaseOrderHistoryRequest {
  /**
   * The cursor of the last entry you read. Leave it empty for the first page.
   *
   * `text`, omittable
   */
  afterCursor?: string;
  /** `uuid` */
  id: Uuid;
  /** `int32` */
  limit: number;
}

/** What `wamn-receiving:receiving/load-purchase-order-history@1.0.0` calls its input members. */
export const RECEIVING_LOAD_PURCHASE_ORDER_HISTORY_REQUEST_FIELDS: FieldMap = {
  "after_cursor": "afterCursor",
  "id": "id",
  "limit": "limit",
};

/** One row of `wamn-receiving:receiving/load-purchase-order-history@1.0.0`. */
export interface ReceivingLoadPurchaseOrderHistoryRow {
  /** `text` */
  readonly after: string;
  /** `text` */
  readonly before: string;
  /** `timestamptz` */
  readonly changedAt: Timestamptz;
  /** `uuid` */
  readonly changedBy: Uuid;
  /** `text` */
  readonly current: string;
  /** `text` */
  readonly cursor: string;
  /** `text` */
  readonly kind: "delete" | "insert" | "update";
  /** `text` */
  readonly operation: string;
}

/** Result of `wamn-receiving:receiving/load-purchase-order-history@1.0.0`. */
export interface ReceivingLoadPurchaseOrderHistoryResult {
  /** Every row the release served. */
  readonly rows: readonly ReceivingLoadPurchaseOrderHistoryRow[];
}

/** What `wamn-receiving:receiving/load-purchase-order-history@1.0.0` calls its result members. */
export const RECEIVING_LOAD_PURCHASE_ORDER_HISTORY_RESULT_FIELDS: FieldMap = {
  "rows": {
    member: "rows",
    fields: {
      "after": "after",
      "before": "before",
      "changed_at": "changedAt",
      "changed_by": "changedBy",
      "current": "current",
      "cursor": "cursor",
      "kind": "kind",
      "operation": "operation",
    },
  },
};

/**
 * Where the release publishes `wamn-receiving:receiving/load-purchase-order-history@1.0.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const RECEIVING_LOAD_PURCHASE_ORDER_HISTORY_ROUTE: OperationRoute = {
  operation: "wamn-receiving:receiving/load-purchase-order-history@1.0.0",
  method: "GET",
  template: "/receiving/load_purchase_order_history",
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

/** Invoke `wamn-receiving:receiving/load-purchase-order-history@1.0.0` through a transport the application supplies. */
export async function loadPurchaseOrderHistory(
  transport: Transport,
  items: readonly ReceivingLoadPurchaseOrderHistoryRequest[],
): Promise<Outcome<ReceivingLoadPurchaseOrderHistoryResult>> {
  return reviveOutcome<ReceivingLoadPurchaseOrderHistoryResult>(
    await transport.invoke({
      ...RECEIVING_LOAD_PURCHASE_ORDER_HISTORY_ROUTE,
      items: items.map((item) => toWire(item, RECEIVING_LOAD_PURCHASE_ORDER_HISTORY_REQUEST_FIELDS)),
    }),
    RECEIVING_LOAD_PURCHASE_ORDER_HISTORY_RESULT_FIELDS,
  );
}

/** Input for `wamn-receiving:receiving/load-receipt-screen@1.0.0`. */
export interface ReceivingLoadReceiptScreenRequest {
  /** `uuid` */
  purchaseOrderId: Uuid;
}

/** What `wamn-receiving:receiving/load-receipt-screen@1.0.0` calls its input members. */
export const RECEIVING_LOAD_RECEIPT_SCREEN_REQUEST_FIELDS: FieldMap = {
  "purchase_order_id": "purchaseOrderId",
};

/** One row of `wamn-receiving:receiving/load-receipt-screen@1.0.0`. */
export interface ReceivingLoadReceiptScreenRow {
  /** `uuid` */
  readonly itemId: Uuid | null;
  /** `text` */
  readonly itemNumber: string | null;
  /** `uuid` */
  readonly lineId: Uuid | null;
  /** `int32` */
  readonly lineNumber: number | null;
  /** `numeric` */
  readonly orderedQuantity: Numeric | null;
  /** `uuid` */
  readonly purchaseOrderId: Uuid;
  /** `text` */
  readonly purchaseOrderNumber: string;
  /** `text` */
  readonly purchaseOrderStatus: "cancelled" | "complete" | "open";
  /** `numeric` */
  readonly receivedQuantity: Numeric | null;
  /** `numeric` */
  readonly remainingQuantity: Numeric | null;
  /** `int32` */
  readonly rowVersion: number;
  /** `uuid` */
  readonly supplierId: Uuid;
}

/** Result of `wamn-receiving:receiving/load-receipt-screen@1.0.0`. */
export interface ReceivingLoadReceiptScreenResult {
  /** Every row the release served. */
  readonly rows: readonly ReceivingLoadReceiptScreenRow[];
}

/** What `wamn-receiving:receiving/load-receipt-screen@1.0.0` calls its result members. */
export const RECEIVING_LOAD_RECEIPT_SCREEN_RESULT_FIELDS: FieldMap = {
  "rows": {
    member: "rows",
    fields: {
      "item_id": "itemId",
      "item_number": "itemNumber",
      "line_id": "lineId",
      "line_number": "lineNumber",
      "ordered_quantity": "orderedQuantity",
      "purchase_order_id": "purchaseOrderId",
      "purchase_order_number": "purchaseOrderNumber",
      "purchase_order_status": "purchaseOrderStatus",
      "received_quantity": "receivedQuantity",
      "remaining_quantity": "remainingQuantity",
      "row_version": "rowVersion",
      "supplier_id": "supplierId",
    },
  },
};

/**
 * Where the release publishes `wamn-receiving:receiving/load-receipt-screen@1.0.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const RECEIVING_LOAD_RECEIPT_SCREEN_ROUTE: OperationRoute = {
  operation: "wamn-receiving:receiving/load-receipt-screen@1.0.0",
  method: "GET",
  template: "/receiving/load_receipt_screen",
  freshOnly: false,
  contract: {
    resultClass: "bounded_list",
    partialSchema: null,
    errors: [
      { literal: "internal_error", required: [], sources: ["query_error", "row_limit_exceeded", "undeclared_constraint"] },
      { literal: "invalid_input", required: ["field"], sources: ["malformed_input"] },
      { literal: "not_found", required: ["field", "id"], sources: ["transaction_invariant"] },
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

/** Invoke `wamn-receiving:receiving/load-receipt-screen@1.0.0` through a transport the application supplies. */
export async function loadReceiptScreen(
  transport: Transport,
  items: readonly ReceivingLoadReceiptScreenRequest[],
): Promise<Outcome<ReceivingLoadReceiptScreenResult>> {
  return reviveOutcome<ReceivingLoadReceiptScreenResult>(
    await transport.invoke({
      ...RECEIVING_LOAD_RECEIPT_SCREEN_ROUTE,
      items: items.map((item) => toWire(item, RECEIVING_LOAD_RECEIPT_SCREEN_REQUEST_FIELDS)),
    }),
    RECEIVING_LOAD_RECEIPT_SCREEN_RESULT_FIELDS,
  );
}

/**
 * Input for `wamn-receiving:receiving/record-receipt@1.0.0`.
 *
 * One submission records every line of one receipt against one purchase order.
 */
export interface ReceivingRecordReceiptRequest {
  /** `text` */
  requestId: string;
  /** `object` */
  value: ReceivingRecordReceiptRequestValue;
}

export interface ReceivingRecordReceiptRequestValue {
  /** `text` */
  idempotencyKey: string;
  /**
   * One line for each order line this receipt records.
   *
   * `array`
   */
  line: ReceivingRecordReceiptRequestValueLine[];
  /** `timestamptz` */
  occurredAt: Timestamptz;
  /** `uuid` */
  purchaseOrderId: Uuid;
  /** `text` */
  receiptReference: string;
}

export interface ReceivingRecordReceiptRequestValueLine {
  /** `uuid` */
  locationId: Uuid;
  /** `uuid` */
  purchaseOrderLineId: Uuid;
  /**
   * More than zero, and no more than the quantity the line still has remaining.
   *
   * `numeric`
   */
  quantity: Numeric;
}

/** What `wamn-receiving:receiving/record-receipt@1.0.0` calls its input members. */
export const RECEIVING_RECORD_RECEIPT_REQUEST_FIELDS: FieldMap = {
  "request_id": "requestId",
  "value": {
    member: "value",
    fields: {
      "idempotency_key": "idempotencyKey",
      "line": {
        member: "line",
        fields: {
          "location_id": "locationId",
          "purchase_order_line_id": "purchaseOrderLineId",
          "quantity": "quantity",
        },
      },
      "occurred_at": "occurredAt",
      "purchase_order_id": "purchaseOrderId",
      "receipt_reference": "receiptReference",
    },
  },
};

/** Result of `wamn-receiving:receiving/record-receipt@1.0.0`. */
export interface ReceivingRecordReceiptResult {
  /** `uuid` */
  readonly purchaseOrderId: Uuid;
  /** `text` */
  readonly purchaseOrderStatus: "complete" | "open";
  /** `uuid` */
  readonly receiptId: Uuid;
  /** `int32` */
  readonly rowVersion: number;
}

/** What `wamn-receiving:receiving/record-receipt@1.0.0` calls its result members. */
export const RECEIVING_RECORD_RECEIPT_RESULT_FIELDS: FieldMap = {
  "purchase_order_id": "purchaseOrderId",
  "purchase_order_status": "purchaseOrderStatus",
  "receipt_id": "receiptId",
  "row_version": "rowVersion",
};

/**
 * Where the release publishes `wamn-receiving:receiving/record-receipt@1.0.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const RECEIVING_RECORD_RECEIPT_ROUTE: OperationRoute = {
  operation: "wamn-receiving:receiving/record-receipt@1.0.0",
  method: "POST",
  template: "/receiving/record_receipt",
  freshOnly: false,
  contract: {
    resultClass: "one",
    partialSchema: null,
    errors: [
      { literal: "idempotency_conflict", required: ["field"], sources: ["same_key_different_canonical_command"] },
      { literal: "internal_error", required: [], sources: ["query_error", "row_limit_exceeded", "undeclared_constraint"] },
      { literal: "invalid_input", required: ["field"], sources: ["duplicate_line", "envelope_count", "line_count", "malformed_input", "nonpositive_quantity"] },
      { literal: "location_not_found", required: ["field", "id"], sources: ["transaction_invariant"] },
      { literal: "permission_denied", required: ["operation"], sources: ["permission_denied"] },
      { literal: "purchase_order_line_mismatch", required: ["field", "id"], sources: ["transaction_invariant"] },
      { literal: "purchase_order_line_not_found", required: ["field", "id"], sources: ["transaction_invariant"] },
      { literal: "purchase_order_not_found", required: ["field"], sources: ["transaction_invariant"] },
      { literal: "purchase_order_not_open", required: ["field"], sources: ["transaction_invariant"] },
      { literal: "quantity_exceeds_remaining", required: ["field", "id"], sources: ["transaction_invariant"] },
      { literal: "receipt_reference_conflict", required: ["constraint"], sources: ["unique_violation"] },
      { literal: "retry", required: [], sources: ["connection_unavailable", "serialization_failure"] },
      { literal: "timeout", required: [], sources: ["statement_timeout"] },
    ],
    replay: "claim",
    direct: true,
    kind: "command",
    transaction: "explicit_per_input",
  },
};

/** Invoke `wamn-receiving:receiving/record-receipt@1.0.0` through a transport the application supplies. */
export async function recordReceipt(
  transport: Transport,
  items: readonly ReceivingRecordReceiptRequest[],
): Promise<Outcome<ReceivingRecordReceiptResult>> {
  return reviveOutcome<ReceivingRecordReceiptResult>(
    await transport.invoke({
      ...RECEIVING_RECORD_RECEIPT_ROUTE,
      items: items.map((item) => toWire(item, RECEIVING_RECORD_RECEIPT_REQUEST_FIELDS)),
    }),
    RECEIVING_RECORD_RECEIPT_RESULT_FIELDS,
  );
}
