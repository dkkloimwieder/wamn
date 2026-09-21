// @generated from the client-contract IR; do not edit.
//
// `receiving` operations of package `wamn_receiving`.

import type { Int64, Numeric, OperationRoute, Outcome, Timestamptz, Transport, Uuid } from "./wire.js";
import { reviveOutcome, toWire } from "./wire.js";

/** Input for `wamn-receiving:receiving/load-purchase-order-history@1.0.0`. */
export interface ReceivingLoadPurchaseOrderHistoryRequest {
  /** `int64` */
  readonly afterPosition: Int64;
  /** `uuid` */
  readonly id: Uuid;
  /** `int64` */
  readonly limit: Int64;
  /** `text` */
  readonly requestId: string;
}

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
  /** `int64` */
  readonly headPosition: Int64;
  /** `text` */
  readonly kind: string;
  /** `text` */
  readonly operation: string;
  /** `int64` */
  readonly position: Int64;
  /** `int64` */
  readonly transactionId: Int64;
}

/** Result of `wamn-receiving:receiving/load-purchase-order-history@1.0.0`. */
export interface ReceivingLoadPurchaseOrderHistoryResult {
  /** Every row the release served. */
  readonly rows: readonly ReceivingLoadPurchaseOrderHistoryRow[];
}

/**
 * Where the release publishes `wamn-receiving:receiving/load-purchase-order-history@1.0.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const RECEIVING_LOAD_PURCHASE_ORDER_HISTORY_ROUTE: OperationRoute = {
  operation: "wamn-receiving:receiving/load-purchase-order-history@1.0.0",
  method: "POST",
  template: "/receiving/load_purchase_order_history",
  freshOnly: false,
  contract: {
    resultClass: "bounded_list",
    partialSchema: null,
    errors: [
      "internal_error",
      "invalid_input",
      "permission_denied",
      "retry",
      "timeout",
    ],
    replay: null,
  },
};

/** Invoke `wamn-receiving:receiving/load-purchase-order-history@1.0.0` through a transport the application supplies. */
export async function loadPurchaseOrderHistory(
  transport: Transport,
  items: readonly ReceivingLoadPurchaseOrderHistoryRequest[],
): Promise<Outcome<ReceivingLoadPurchaseOrderHistoryResult>> {
  return reviveOutcome<ReceivingLoadPurchaseOrderHistoryResult>(
    await transport.invoke({ ...RECEIVING_LOAD_PURCHASE_ORDER_HISTORY_ROUTE, items: items.map(toWire) }),
  );
}

/** Input for `wamn-receiving:receiving/load-receipt-screen@1.0.0`. */
export interface ReceivingLoadReceiptScreenRequest {
  /** `uuid` */
  readonly purchaseOrderId: Uuid;
  /** `text` */
  readonly requestId: string;
}

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
  readonly purchaseOrderStatus: string;
  /** `numeric` */
  readonly receivedQuantity: Numeric | null;
  /** `numeric` */
  readonly remainingQuantity: Numeric | null;
  /** `int64` */
  readonly rowVersion: Int64;
  /** `uuid` */
  readonly supplierId: Uuid;
}

/** Result of `wamn-receiving:receiving/load-receipt-screen@1.0.0`. */
export interface ReceivingLoadReceiptScreenResult {
  /** Every row the release served. */
  readonly rows: readonly ReceivingLoadReceiptScreenRow[];
}

/**
 * Where the release publishes `wamn-receiving:receiving/load-receipt-screen@1.0.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const RECEIVING_LOAD_RECEIPT_SCREEN_ROUTE: OperationRoute = {
  operation: "wamn-receiving:receiving/load-receipt-screen@1.0.0",
  method: "POST",
  template: "/receiving/load_receipt_screen",
  freshOnly: false,
  contract: {
    resultClass: "bounded_list",
    partialSchema: null,
    errors: [
      "internal_error",
      "invalid_input",
      "not_found",
      "permission_denied",
      "retry",
      "timeout",
    ],
    replay: null,
  },
};

/** Invoke `wamn-receiving:receiving/load-receipt-screen@1.0.0` through a transport the application supplies. */
export async function loadReceiptScreen(
  transport: Transport,
  items: readonly ReceivingLoadReceiptScreenRequest[],
): Promise<Outcome<ReceivingLoadReceiptScreenResult>> {
  return reviveOutcome<ReceivingLoadReceiptScreenResult>(
    await transport.invoke({ ...RECEIVING_LOAD_RECEIPT_SCREEN_ROUTE, items: items.map(toWire) }),
  );
}

/** Input for `wamn-receiving:receiving/record-receipt@1.0.0`. */
export interface ReceivingRecordReceiptRequest {
  /** `text` */
  readonly requestId: string;
  /** `object` */
  readonly value: ReceivingRecordReceiptRequestValue;
}

export interface ReceivingRecordReceiptRequestValue {
  /** `text` */
  readonly idempotencyKey: string;
  /** `array` */
  readonly line: readonly ReceivingRecordReceiptRequestValueLine[];
  /** `timestamptz` */
  readonly occurredAt: Timestamptz;
  /** `uuid` */
  readonly purchaseOrderId: Uuid;
  /** `text` */
  readonly receiptReference: string;
}

export interface ReceivingRecordReceiptRequestValueLine {
  /** `uuid` */
  readonly locationId: Uuid;
  /** `uuid` */
  readonly purchaseOrderLineId: Uuid;
  /** `numeric` */
  readonly quantity: Numeric;
}

/** Result of `wamn-receiving:receiving/record-receipt@1.0.0`. */
export interface ReceivingRecordReceiptResult {
  /** `uuid` */
  readonly purchaseOrderId: Uuid;
  /** `text` */
  readonly purchaseOrderStatus: string;
  /** `uuid` */
  readonly receiptId: Uuid;
  /** `int64` */
  readonly rowVersion: Int64;
}

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
      "idempotency_conflict",
      "internal_error",
      "invalid_input",
      "location_not_found",
      "permission_denied",
      "purchase_order_line_mismatch",
      "purchase_order_line_not_found",
      "purchase_order_not_found",
      "purchase_order_not_open",
      "quantity_exceeds_remaining",
      "receipt_reference_conflict",
      "retry",
      "timeout",
    ],
    replay: "claim",
  },
};

/** Invoke `wamn-receiving:receiving/record-receipt@1.0.0` through a transport the application supplies. */
export async function recordReceipt(
  transport: Transport,
  items: readonly ReceivingRecordReceiptRequest[],
): Promise<Outcome<ReceivingRecordReceiptResult>> {
  return reviveOutcome<ReceivingRecordReceiptResult>(
    await transport.invoke({ ...RECEIVING_RECORD_RECEIPT_ROUTE, items: items.map(toWire) }),
  );
}
