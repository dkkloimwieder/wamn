// @generated from the client-contract IR; do not edit.
//
// `pallet_quantity` operations of package `wamn_wms`.

import type { FieldMap, Numeric, OperationRoute, Outcome, Timestamptz, Transport, Uuid } from "@wamn/web-runtime";
import { reviveOutcome, toWire } from "@wamn/web-runtime";

/** Input for `wamn-wms:pallet-quantity/get@1.0.0`. */
export interface PalletQuantityGetRequest {
  /** `uuid` */
  id: Uuid;
  /** `string` */
  requestId: string;
}

/** What `wamn-wms:pallet-quantity/get@1.0.0` calls its input members. */
export const PALLET_QUANTITY_GET_REQUEST_FIELDS: FieldMap = {
  "id": "id",
  "request_id": "requestId",
};

/** Result of `wamn-wms:pallet-quantity/get@1.0.0`. */
export interface PalletQuantityGetResult {
  /** `timestamptz` */
  readonly createdAt: Timestamptz;
  /** `uuid` */
  readonly id: Uuid;
  /** `uuid` */
  readonly palletId: Uuid;
  /** `uuid` */
  readonly productId: Uuid;
  /** `numeric` */
  readonly quantity: Numeric;
  /** `text` */
  readonly status: string;
}

/** What `wamn-wms:pallet-quantity/get@1.0.0` calls its result members. */
export const PALLET_QUANTITY_GET_RESULT_FIELDS: FieldMap = {
  "created_at": "createdAt",
  "id": "id",
  "pallet_id": "palletId",
  "product_id": "productId",
  "quantity": "quantity",
  "status": "status",
};

/**
 * Where the release publishes `wamn-wms:pallet-quantity/get@1.0.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const PALLET_QUANTITY_GET_ROUTE: OperationRoute = {
  operation: "wamn-wms:pallet-quantity/get@1.0.0",
  method: "POST",
  template: "/pallet_quantity/get",
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

/** Invoke `wamn-wms:pallet-quantity/get@1.0.0` through a transport the application supplies. */
export async function get(
  transport: Transport,
  items: readonly PalletQuantityGetRequest[],
): Promise<Outcome<PalletQuantityGetResult>> {
  return reviveOutcome<PalletQuantityGetResult>(
    await transport.invoke({
      ...PALLET_QUANTITY_GET_ROUTE,
      items: items.map((item) => toWire(item, PALLET_QUANTITY_GET_REQUEST_FIELDS)),
    }),
    PALLET_QUANTITY_GET_RESULT_FIELDS,
  );
}

/** Input for `wamn-wms:pallet-quantity/query@1.0.0`. */
export interface PalletQuantityQueryRequest {
  /** `text`, omittable */
  cursor?: string;
  /** `int32`, omittable */
  limit?: number;
  /** `string` */
  requestId: string;
}

/** What `wamn-wms:pallet-quantity/query@1.0.0` calls its input members. */
export const PALLET_QUANTITY_QUERY_REQUEST_FIELDS: FieldMap = {
  "cursor": "cursor",
  "limit": "limit",
  "request_id": "requestId",
};

/** One row of `wamn-wms:pallet-quantity/query@1.0.0`. */
export interface PalletQuantityQueryRow {
  /** `timestamptz` */
  readonly createdAt: Timestamptz;
  /** `uuid` */
  readonly id: Uuid;
  /** `uuid` */
  readonly palletId: Uuid;
  /** `uuid` */
  readonly productId: Uuid;
  /** `numeric` */
  readonly quantity: Numeric;
  /** `text` */
  readonly status: string;
}

/** Result of `wamn-wms:pallet-quantity/query@1.0.0`. */
export interface PalletQuantityQueryResult {
  /** The rows this page carries. */
  readonly item: readonly PalletQuantityQueryRow[];
  /** The next page's cursor, or null at the last page. */
  readonly nextCursor: string | null;
}

/** What `wamn-wms:pallet-quantity/query@1.0.0` calls its result members. */
export const PALLET_QUANTITY_QUERY_RESULT_FIELDS: FieldMap = {
  "item": {
    member: "item",
    fields: {
      "created_at": "createdAt",
      "id": "id",
      "pallet_id": "palletId",
      "product_id": "productId",
      "quantity": "quantity",
      "status": "status",
    },
  },
  "next_cursor": "nextCursor",
};

/**
 * Where the release publishes `wamn-wms:pallet-quantity/query@1.0.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const PALLET_QUANTITY_QUERY_ROUTE: OperationRoute = {
  operation: "wamn-wms:pallet-quantity/query@1.0.0",
  method: "POST",
  template: "/pallet_quantity/query",
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

/** Invoke `wamn-wms:pallet-quantity/query@1.0.0` through a transport the application supplies. */
export async function query(
  transport: Transport,
  items: readonly PalletQuantityQueryRequest[],
): Promise<Outcome<PalletQuantityQueryResult>> {
  return reviveOutcome<PalletQuantityQueryResult>(
    await transport.invoke({
      ...PALLET_QUANTITY_QUERY_ROUTE,
      items: items.map((item) => toWire(item, PALLET_QUANTITY_QUERY_REQUEST_FIELDS)),
    }),
    PALLET_QUANTITY_QUERY_RESULT_FIELDS,
  );
}
