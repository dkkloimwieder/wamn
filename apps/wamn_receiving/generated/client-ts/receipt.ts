// @generated from the client-contract IR; do not edit.
//
// `receipt` operations of package `wamn_receiving`.

import type { Int64, OperationRoute, Outcome, Timestamptz, Transport, Uuid } from "./wire.js";
import { reviveOutcome, toWire } from "./wire.js";

/** Input for `wamn-receiving:receipt/get@1.0.0`. */
export interface ReceiptGetRequest {
  /** `uuid` */
  readonly id: Uuid;
  /** `string` */
  readonly requestId: string;
}

/** Result of `wamn-receiving:receipt/get@1.0.0`. */
export interface ReceiptGetResult {
  /** `timestamptz` */
  readonly createdAt: Timestamptz;
  /** `uuid` */
  readonly createdBy: Uuid;
  /** `uuid` */
  readonly id: Uuid;
  /** `text` */
  readonly idempotencyKey: string;
  /** `timestamptz` */
  readonly occurredAt: Timestamptz;
  /** `uuid` */
  readonly purchaseOrderId: Uuid;
  /** `text` */
  readonly receiptReference: string;
}

/**
 * Where the release publishes `wamn-receiving:receipt/get@1.0.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const RECEIPT_GET_ROUTE: OperationRoute = {
  operation: "wamn-receiving:receipt/get@1.0.0",
  method: "POST",
  template: "/receipt/get",
  freshOnly: false,
  contract: {
    resultClass: "one",
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

/** Invoke `wamn-receiving:receipt/get@1.0.0` through a transport the application supplies. */
export async function get(
  transport: Transport,
  items: readonly ReceiptGetRequest[],
): Promise<Outcome<ReceiptGetResult>> {
  return reviveOutcome<ReceiptGetResult>(
    await transport.invoke({ ...RECEIPT_GET_ROUTE, items: items.map(toWire) }),
  );
}

/** Input for `wamn-receiving:receipt/query@1.0.0`. */
export interface ReceiptQueryRequest {
  /** `text`, omittable */
  readonly cursor?: string;
  /** `int64`, omittable */
  readonly limit?: Int64;
  /** `string` */
  readonly requestId: string;
}

/** One row of `wamn-receiving:receipt/query@1.0.0`. */
export interface ReceiptQueryRow {
  /** `timestamptz` */
  readonly createdAt: Timestamptz;
  /** `uuid` */
  readonly createdBy: Uuid;
  /** `uuid` */
  readonly id: Uuid;
  /** `text` */
  readonly idempotencyKey: string;
  /** `timestamptz` */
  readonly occurredAt: Timestamptz;
  /** `uuid` */
  readonly purchaseOrderId: Uuid;
  /** `text` */
  readonly receiptReference: string;
}

/** Result of `wamn-receiving:receipt/query@1.0.0`. */
export interface ReceiptQueryResult {
  /** The rows this page carries. */
  readonly item: readonly ReceiptQueryRow[];
  /** The next page's cursor, or null at the last page. */
  readonly nextCursor: string | null;
}

/**
 * Where the release publishes `wamn-receiving:receipt/query@1.0.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const RECEIPT_QUERY_ROUTE: OperationRoute = {
  operation: "wamn-receiving:receipt/query@1.0.0",
  method: "POST",
  template: "/receipt/query",
  freshOnly: false,
  contract: {
    resultClass: "page",
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

/** Invoke `wamn-receiving:receipt/query@1.0.0` through a transport the application supplies. */
export async function query(
  transport: Transport,
  items: readonly ReceiptQueryRequest[],
): Promise<Outcome<ReceiptQueryResult>> {
  return reviveOutcome<ReceiptQueryResult>(
    await transport.invoke({ ...RECEIPT_QUERY_ROUTE, items: items.map(toWire) }),
  );
}
