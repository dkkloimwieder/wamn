// @generated from the client-contract IR; do not edit.
//
// `receipt` operations of package `wamn_receiving`.

import type { FieldMap, OperationRoute, Outcome, Timestamptz, Transport, Uuid } from "@wamn/web-runtime";
import { reviveOutcome, toWire } from "@wamn/web-runtime";

/** Input for `wamn-receiving:receipt/get@1.0.0`. */
export interface ReceiptGetRequest {
  /** `uuid` */
  id: Uuid;
}

/** What `wamn-receiving:receipt/get@1.0.0` calls its input members. */
export const RECEIPT_GET_REQUEST_FIELDS: FieldMap = {
  "id": "id",
};

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

/** What `wamn-receiving:receipt/get@1.0.0` calls its result members. */
export const RECEIPT_GET_RESULT_FIELDS: FieldMap = {
  "created_at": "createdAt",
  "created_by": "createdBy",
  "id": "id",
  "idempotency_key": "idempotencyKey",
  "occurred_at": "occurredAt",
  "purchase_order_id": "purchaseOrderId",
  "receipt_reference": "receiptReference",
};

/**
 * Where the release publishes `wamn-receiving:receipt/get@1.0.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const RECEIPT_GET_ROUTE: OperationRoute = {
  operation: "wamn-receiving:receipt/get@1.0.0",
  method: "GET",
  template: "/receipt/get",
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

/** Invoke `wamn-receiving:receipt/get@1.0.0` through a transport the application supplies. */
export async function get(
  transport: Transport,
  items: readonly ReceiptGetRequest[],
): Promise<Outcome<ReceiptGetResult>> {
  return reviveOutcome<ReceiptGetResult>(
    await transport.invoke({
      ...RECEIPT_GET_ROUTE,
      items: items.map((item) => toWire(item, RECEIPT_GET_REQUEST_FIELDS)),
    }),
    RECEIPT_GET_RESULT_FIELDS,
  );
}

/** Input for `wamn-receiving:receipt/query@1.0.0`. */
export interface ReceiptQueryRequest {
  /** `text`, omittable */
  cursor?: string;
  /** `int32`, omittable */
  limit?: number;
}

/** What `wamn-receiving:receipt/query@1.0.0` calls its input members. */
export const RECEIPT_QUERY_REQUEST_FIELDS: FieldMap = {
  "cursor": "cursor",
  "limit": "limit",
};

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

/** What `wamn-receiving:receipt/query@1.0.0` calls its result members. */
export const RECEIPT_QUERY_RESULT_FIELDS: FieldMap = {
  "item": {
    member: "item",
    fields: {
      "created_at": "createdAt",
      "created_by": "createdBy",
      "id": "id",
      "idempotency_key": "idempotencyKey",
      "occurred_at": "occurredAt",
      "purchase_order_id": "purchaseOrderId",
      "receipt_reference": "receiptReference",
    },
  },
  "next_cursor": "nextCursor",
};

/**
 * Where the release publishes `wamn-receiving:receipt/query@1.0.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const RECEIPT_QUERY_ROUTE: OperationRoute = {
  operation: "wamn-receiving:receipt/query@1.0.0",
  method: "GET",
  template: "/receipt/query",
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

/** Invoke `wamn-receiving:receipt/query@1.0.0` through a transport the application supplies. */
export async function query(
  transport: Transport,
  items: readonly ReceiptQueryRequest[],
): Promise<Outcome<ReceiptQueryResult>> {
  return reviveOutcome<ReceiptQueryResult>(
    await transport.invoke({
      ...RECEIPT_QUERY_ROUTE,
      items: items.map((item) => toWire(item, RECEIPT_QUERY_REQUEST_FIELDS)),
    }),
    RECEIPT_QUERY_RESULT_FIELDS,
  );
}
