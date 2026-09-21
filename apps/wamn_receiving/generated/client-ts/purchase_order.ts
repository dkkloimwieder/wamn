// @generated from the client-contract IR; do not edit.
//
// `purchase_order` operations of package `wamn_receiving`.

import type { FieldMap, Int64, OperationRoute, Outcome, Timestamptz, Transport, Uuid } from "./wire.js";
import { reviveOutcome, toWire } from "./wire.js";

/** Input for `wamn-receiving:purchase-order/get@1.0.0`. */
export interface PurchaseOrderGetRequest {
  /** `uuid` */
  readonly id: Uuid;
  /** `string` */
  readonly requestId: string;
}

/** What `wamn-receiving:purchase-order/get@1.0.0` calls its input members. */
export const PURCHASE_ORDER_GET_REQUEST_FIELDS: FieldMap = {
  "id": "id",
  "request_id": "requestId",
};

/** Result of `wamn-receiving:purchase-order/get@1.0.0`. */
export interface PurchaseOrderGetResult {
  /** `timestamptz` */
  readonly createdAt: Timestamptz;
  /** `uuid` */
  readonly createdBy: Uuid;
  /** `uuid` */
  readonly id: Uuid;
  /** `text` */
  readonly purchaseOrderNumber: string;
  /** `int64` */
  readonly rowVersion: Int64;
  /** `text` */
  readonly status: string;
  /** `uuid` */
  readonly supplierId: Uuid;
  /** `timestamptz` */
  readonly updatedAt: Timestamptz;
  /** `uuid` */
  readonly updatedBy: Uuid;
}

/** What `wamn-receiving:purchase-order/get@1.0.0` calls its result members. */
export const PURCHASE_ORDER_GET_RESULT_FIELDS: FieldMap = {
  "created_at": "createdAt",
  "created_by": "createdBy",
  "id": "id",
  "purchase_order_number": "purchaseOrderNumber",
  "row_version": "rowVersion",
  "status": "status",
  "supplier_id": "supplierId",
  "updated_at": "updatedAt",
  "updated_by": "updatedBy",
};

/**
 * Where the release publishes `wamn-receiving:purchase-order/get@1.0.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const PURCHASE_ORDER_GET_ROUTE: OperationRoute = {
  operation: "wamn-receiving:purchase-order/get@1.0.0",
  method: "POST",
  template: "/purchase_order/get",
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

/** Invoke `wamn-receiving:purchase-order/get@1.0.0` through a transport the application supplies. */
export async function get(
  transport: Transport,
  items: readonly PurchaseOrderGetRequest[],
): Promise<Outcome<PurchaseOrderGetResult>> {
  return reviveOutcome<PurchaseOrderGetResult>(
    await transport.invoke({
      ...PURCHASE_ORDER_GET_ROUTE,
      items: items.map((item) => toWire(item, PURCHASE_ORDER_GET_REQUEST_FIELDS)),
    }),
    PURCHASE_ORDER_GET_RESULT_FIELDS,
  );
}

/** Input for `wamn-receiving:purchase-order/query@1.0.0`. */
export interface PurchaseOrderQueryRequest {
  /** `text`, omittable */
  readonly cursor?: string;
  /** `object`, omittable */
  readonly filter?: PurchaseOrderQueryRequestFilter;
  /** `int64`, omittable */
  readonly limit?: Int64;
  /** `string` */
  readonly requestId: string;
  /** `object`, omittable */
  readonly sort?: PurchaseOrderQueryRequestSort;
}

export interface PurchaseOrderQueryRequestFilter {
  /** `array`, omittable */
  readonly status?: readonly string[];
  /** `array`, omittable */
  readonly supplierId?: readonly Uuid[];
}

export interface PurchaseOrderQueryRequestSort {
  /** `text` */
  readonly direction: string;
  /** `text` */
  readonly field: string;
}

/** What `wamn-receiving:purchase-order/query@1.0.0` calls its input members. */
export const PURCHASE_ORDER_QUERY_REQUEST_FIELDS: FieldMap = {
  "cursor": "cursor",
  "filter": {
    member: "filter",
    fields: {
      "status": "status",
      "supplier_id": "supplierId",
    },
  },
  "limit": "limit",
  "request_id": "requestId",
  "sort": {
    member: "sort",
    fields: {
      "direction": "direction",
      "field": "field",
    },
  },
};

/** One row of `wamn-receiving:purchase-order/query@1.0.0`. */
export interface PurchaseOrderQueryRow {
  /** `timestamptz` */
  readonly createdAt: Timestamptz;
  /** `uuid` */
  readonly createdBy: Uuid;
  /** `uuid` */
  readonly id: Uuid;
  /** `text` */
  readonly purchaseOrderNumber: string;
  /** `int64` */
  readonly rowVersion: Int64;
  /** `text` */
  readonly status: string;
  /** `uuid` */
  readonly supplierId: Uuid;
  /** `timestamptz` */
  readonly updatedAt: Timestamptz;
  /** `uuid` */
  readonly updatedBy: Uuid;
}

/** Result of `wamn-receiving:purchase-order/query@1.0.0`. */
export interface PurchaseOrderQueryResult {
  /** The rows this page carries. */
  readonly item: readonly PurchaseOrderQueryRow[];
  /** The next page's cursor, or null at the last page. */
  readonly nextCursor: string | null;
}

/** What `wamn-receiving:purchase-order/query@1.0.0` calls its result members. */
export const PURCHASE_ORDER_QUERY_RESULT_FIELDS: FieldMap = {
  "item": {
    member: "item",
    fields: {
      "created_at": "createdAt",
      "created_by": "createdBy",
      "id": "id",
      "purchase_order_number": "purchaseOrderNumber",
      "row_version": "rowVersion",
      "status": "status",
      "supplier_id": "supplierId",
      "updated_at": "updatedAt",
      "updated_by": "updatedBy",
    },
  },
  "next_cursor": "nextCursor",
};

/**
 * Where the release publishes `wamn-receiving:purchase-order/query@1.0.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const PURCHASE_ORDER_QUERY_ROUTE: OperationRoute = {
  operation: "wamn-receiving:purchase-order/query@1.0.0",
  method: "POST",
  template: "/purchase_order/query",
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

/** Invoke `wamn-receiving:purchase-order/query@1.0.0` through a transport the application supplies. */
export async function query(
  transport: Transport,
  items: readonly PurchaseOrderQueryRequest[],
): Promise<Outcome<PurchaseOrderQueryResult>> {
  return reviveOutcome<PurchaseOrderQueryResult>(
    await transport.invoke({
      ...PURCHASE_ORDER_QUERY_ROUTE,
      items: items.map((item) => toWire(item, PURCHASE_ORDER_QUERY_REQUEST_FIELDS)),
    }),
    PURCHASE_ORDER_QUERY_RESULT_FIELDS,
  );
}

/** Input for `wamn-receiving:purchase-order/update@1.0.0`. */
export interface PurchaseOrderUpdateRequest {
  /** `object` */
  readonly change: PurchaseOrderUpdateRequestChange;
  /** `int64` */
  readonly expectedRowVersion: Int64;
  /** `uuid` */
  readonly id: Uuid;
  /** `string` */
  readonly requestId: string;
}

export interface PurchaseOrderUpdateRequestChange {
  /** `uuid`, omittable */
  readonly supplierId?: Uuid;
}

/** What `wamn-receiving:purchase-order/update@1.0.0` calls its input members. */
export const PURCHASE_ORDER_UPDATE_REQUEST_FIELDS: FieldMap = {
  "change": {
    member: "change",
    fields: {
      "supplier_id": "supplierId",
    },
  },
  "expected_row_version": "expectedRowVersion",
  "id": "id",
  "request_id": "requestId",
};

/** Result of `wamn-receiving:purchase-order/update@1.0.0`. */
export interface PurchaseOrderUpdateResult {
  /** `timestamptz` */
  readonly createdAt: Timestamptz;
  /** `uuid` */
  readonly createdBy: Uuid;
  /** `uuid` */
  readonly id: Uuid;
  /** `text` */
  readonly purchaseOrderNumber: string;
  /** `int64` */
  readonly rowVersion: Int64;
  /** `text` */
  readonly status: string;
  /** `uuid` */
  readonly supplierId: Uuid;
  /** `timestamptz` */
  readonly updatedAt: Timestamptz;
  /** `uuid` */
  readonly updatedBy: Uuid;
}

/** What `wamn-receiving:purchase-order/update@1.0.0` calls its result members. */
export const PURCHASE_ORDER_UPDATE_RESULT_FIELDS: FieldMap = {
  "created_at": "createdAt",
  "created_by": "createdBy",
  "id": "id",
  "purchase_order_number": "purchaseOrderNumber",
  "row_version": "rowVersion",
  "status": "status",
  "supplier_id": "supplierId",
  "updated_at": "updatedAt",
  "updated_by": "updatedBy",
};

/**
 * Where the release publishes `wamn-receiving:purchase-order/update@1.0.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const PURCHASE_ORDER_UPDATE_ROUTE: OperationRoute = {
  operation: "wamn-receiving:purchase-order/update@1.0.0",
  method: "POST",
  template: "/purchase_order/update",
  freshOnly: false,
  contract: {
    resultClass: "one",
    partialSchema: null,
    errors: [
      "concurrency_conflict",
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

/** Invoke `wamn-receiving:purchase-order/update@1.0.0` through a transport the application supplies. */
export async function update(
  transport: Transport,
  items: readonly PurchaseOrderUpdateRequest[],
): Promise<Outcome<PurchaseOrderUpdateResult>> {
  return reviveOutcome<PurchaseOrderUpdateResult>(
    await transport.invoke({
      ...PURCHASE_ORDER_UPDATE_ROUTE,
      items: items.map((item) => toWire(item, PURCHASE_ORDER_UPDATE_REQUEST_FIELDS)),
    }),
    PURCHASE_ORDER_UPDATE_RESULT_FIELDS,
  );
}
