// @generated from the client-contract IR; do not edit.
//
// `packaging_quantity` operations of package `wamn_wms`.

import type { FieldMap, Numeric, OperationRoute, Outcome, Timestamptz, Transport, Uuid } from "@wamn/web-runtime";
import { reviveOutcome, toWire } from "@wamn/web-runtime";

/** Input for `wamn-wms:packaging-quantity/get@1.0.0`. */
export interface PackagingQuantityGetRequest {
  /** `uuid` */
  id: Uuid;
}

/** What `wamn-wms:packaging-quantity/get@1.0.0` calls its input members. */
export const PACKAGING_QUANTITY_GET_REQUEST_FIELDS: FieldMap = {
  "id": "id",
};

/** Result of `wamn-wms:packaging-quantity/get@1.0.0`. */
export interface PackagingQuantityGetResult {
  /** `timestamptz` */
  readonly createdAt: Timestamptz;
  /** `uuid` */
  readonly id: Uuid;
  /** `uuid` */
  readonly packagingId: Uuid;
  /** `uuid` */
  readonly productId: Uuid;
  /** `numeric` */
  readonly quantity: Numeric;
  /** `text` */
  readonly status: string;
}

/** What `wamn-wms:packaging-quantity/get@1.0.0` calls its result members. */
export const PACKAGING_QUANTITY_GET_RESULT_FIELDS: FieldMap = {
  "created_at": "createdAt",
  "id": "id",
  "packaging_id": "packagingId",
  "product_id": "productId",
  "quantity": "quantity",
  "status": "status",
};

/**
 * Where the release publishes `wamn-wms:packaging-quantity/get@1.0.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const PACKAGING_QUANTITY_GET_ROUTE: OperationRoute = {
  operation: "wamn-wms:packaging-quantity/get@1.0.0",
  method: "GET",
  template: "/packaging_quantity/get",
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

/** Invoke `wamn-wms:packaging-quantity/get@1.0.0` through a transport the application supplies. */
export async function get(
  transport: Transport,
  items: readonly PackagingQuantityGetRequest[],
): Promise<Outcome<PackagingQuantityGetResult>> {
  return reviveOutcome<PackagingQuantityGetResult>(
    await transport.invoke({
      ...PACKAGING_QUANTITY_GET_ROUTE,
      items: items.map((item) => toWire(item, PACKAGING_QUANTITY_GET_REQUEST_FIELDS)),
    }),
    PACKAGING_QUANTITY_GET_RESULT_FIELDS,
  );
}

/** Input for `wamn-wms:packaging-quantity/query@1.0.0`. */
export interface PackagingQuantityQueryRequest {
  /** `text`, omittable */
  cursor?: string;
  /** `int32`, omittable */
  limit?: number;
}

/** What `wamn-wms:packaging-quantity/query@1.0.0` calls its input members. */
export const PACKAGING_QUANTITY_QUERY_REQUEST_FIELDS: FieldMap = {
  "cursor": "cursor",
  "limit": "limit",
};

/** One row of `wamn-wms:packaging-quantity/query@1.0.0`. */
export interface PackagingQuantityQueryRow {
  /** `timestamptz` */
  readonly createdAt: Timestamptz;
  /** `uuid` */
  readonly id: Uuid;
  /** `uuid` */
  readonly packagingId: Uuid;
  /** `uuid` */
  readonly productId: Uuid;
  /** `numeric` */
  readonly quantity: Numeric;
  /** `text` */
  readonly status: string;
}

/** Result of `wamn-wms:packaging-quantity/query@1.0.0`. */
export interface PackagingQuantityQueryResult {
  /** The rows this page carries. */
  readonly item: readonly PackagingQuantityQueryRow[];
  /** The next page's cursor, or null at the last page. */
  readonly nextCursor: string | null;
}

/** What `wamn-wms:packaging-quantity/query@1.0.0` calls its result members. */
export const PACKAGING_QUANTITY_QUERY_RESULT_FIELDS: FieldMap = {
  "item": {
    member: "item",
    fields: {
      "created_at": "createdAt",
      "id": "id",
      "packaging_id": "packagingId",
      "product_id": "productId",
      "quantity": "quantity",
      "status": "status",
    },
  },
  "next_cursor": "nextCursor",
};

/**
 * Where the release publishes `wamn-wms:packaging-quantity/query@1.0.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const PACKAGING_QUANTITY_QUERY_ROUTE: OperationRoute = {
  operation: "wamn-wms:packaging-quantity/query@1.0.0",
  method: "GET",
  template: "/packaging_quantity/query",
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

/** Invoke `wamn-wms:packaging-quantity/query@1.0.0` through a transport the application supplies. */
export async function query(
  transport: Transport,
  items: readonly PackagingQuantityQueryRequest[],
): Promise<Outcome<PackagingQuantityQueryResult>> {
  return reviveOutcome<PackagingQuantityQueryResult>(
    await transport.invoke({
      ...PACKAGING_QUANTITY_QUERY_ROUTE,
      items: items.map((item) => toWire(item, PACKAGING_QUANTITY_QUERY_REQUEST_FIELDS)),
    }),
    PACKAGING_QUANTITY_QUERY_RESULT_FIELDS,
  );
}
