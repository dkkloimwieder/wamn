// @generated from the client-contract IR; do not edit.
//
// `product` operations of package `wamn_wms`.

import type { FieldMap, OperationRoute, Outcome, Timestamptz, Transport, Uuid } from "@wamn/web-runtime";
import { reviveOutcome, toWire } from "@wamn/web-runtime";

/** Input for `wamn-wms:product/create@1.0.0`. */
export interface ProductCreateRequest {
  /** `text` */
  idempotencyKey: string;
  /** `text` */
  productCode: string;
  /** `string` */
  requestId: string;
}

/** What `wamn-wms:product/create@1.0.0` calls its input members. */
export const PRODUCT_CREATE_REQUEST_FIELDS: FieldMap = {
  "idempotency_key": "idempotencyKey",
  "product_code": "productCode",
  "request_id": "requestId",
};

/** Result of `wamn-wms:product/create@1.0.0`. */
export interface ProductCreateResult {
  /** `timestamptz` */
  readonly createdAt: Timestamptz;
  /** `uuid` */
  readonly id: Uuid;
  /** `text` */
  readonly productCode: string;
  /** `int32` */
  readonly rowVersion: number;
}

/** What `wamn-wms:product/create@1.0.0` calls its result members. */
export const PRODUCT_CREATE_RESULT_FIELDS: FieldMap = {
  "created_at": "createdAt",
  "id": "id",
  "product_code": "productCode",
  "row_version": "rowVersion",
};

/**
 * Where the release publishes `wamn-wms:product/create@1.0.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const PRODUCT_CREATE_ROUTE: OperationRoute = {
  operation: "wamn-wms:product/create@1.0.0",
  method: "POST",
  template: "/product/create",
  freshOnly: false,
  contract: {
    resultClass: "one",
    partialSchema: null,
    errors: [
      { literal: "idempotency_conflict", required: ["field"], sources: ["changed_canonical_command"] },
      { literal: "internal_error", required: [], sources: ["query_error", "row_limit_exceeded"] },
      { literal: "invalid_input", required: ["field"], sources: [] },
      { literal: "permission_denied", required: ["operation"], sources: ["permission_denied"] },
      { literal: "retry", required: [], sources: ["connection_unavailable", "serialization_failure"] },
      { literal: "timeout", required: [], sources: ["statement_timeout"] },
      { literal: "unique_violation", required: ["constraint"], sources: ["unique_violation"] },
    ],
    replay: "claim",
    direct: true,
    kind: "create",
    transaction: "explicit_per_input",
  },
};

/** Invoke `wamn-wms:product/create@1.0.0` through a transport the application supplies. */
export async function create(
  transport: Transport,
  items: readonly ProductCreateRequest[],
): Promise<Outcome<ProductCreateResult>> {
  return reviveOutcome<ProductCreateResult>(
    await transport.invoke({
      ...PRODUCT_CREATE_ROUTE,
      items: items.map((item) => toWire(item, PRODUCT_CREATE_REQUEST_FIELDS)),
    }),
    PRODUCT_CREATE_RESULT_FIELDS,
  );
}

/** Input for `wamn-wms:product/get@1.0.0`. */
export interface ProductGetRequest {
  /** `uuid` */
  id: Uuid;
  /** `string` */
  requestId: string;
}

/** What `wamn-wms:product/get@1.0.0` calls its input members. */
export const PRODUCT_GET_REQUEST_FIELDS: FieldMap = {
  "id": "id",
  "request_id": "requestId",
};

/** Result of `wamn-wms:product/get@1.0.0`. */
export interface ProductGetResult {
  /** `timestamptz` */
  readonly createdAt: Timestamptz;
  /** `uuid` */
  readonly id: Uuid;
  /** `text` */
  readonly productCode: string;
  /** `int32` */
  readonly rowVersion: number;
}

/** What `wamn-wms:product/get@1.0.0` calls its result members. */
export const PRODUCT_GET_RESULT_FIELDS: FieldMap = {
  "created_at": "createdAt",
  "id": "id",
  "product_code": "productCode",
  "row_version": "rowVersion",
};

/**
 * Where the release publishes `wamn-wms:product/get@1.0.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const PRODUCT_GET_ROUTE: OperationRoute = {
  operation: "wamn-wms:product/get@1.0.0",
  method: "POST",
  template: "/product/get",
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

/** Invoke `wamn-wms:product/get@1.0.0` through a transport the application supplies. */
export async function get(
  transport: Transport,
  items: readonly ProductGetRequest[],
): Promise<Outcome<ProductGetResult>> {
  return reviveOutcome<ProductGetResult>(
    await transport.invoke({
      ...PRODUCT_GET_ROUTE,
      items: items.map((item) => toWire(item, PRODUCT_GET_REQUEST_FIELDS)),
    }),
    PRODUCT_GET_RESULT_FIELDS,
  );
}

/** Input for `wamn-wms:product/query@1.0.0`. */
export interface ProductQueryRequest {
  /** `text`, omittable */
  cursor?: string;
  /** `object`, omittable */
  filter?: ProductQueryRequestFilter;
  /** `int32`, omittable */
  limit?: number;
  /** `string` */
  requestId: string;
}

export interface ProductQueryRequestFilter {
  /** `array`, omittable */
  productCode?: string[];
}

/** What `wamn-wms:product/query@1.0.0` calls its input members. */
export const PRODUCT_QUERY_REQUEST_FIELDS: FieldMap = {
  "cursor": "cursor",
  "filter": {
    member: "filter",
    fields: {
      "product_code": "productCode",
    },
  },
  "limit": "limit",
  "request_id": "requestId",
};

/** One row of `wamn-wms:product/query@1.0.0`. */
export interface ProductQueryRow {
  /** `timestamptz` */
  readonly createdAt: Timestamptz;
  /** `uuid` */
  readonly id: Uuid;
  /** `text` */
  readonly productCode: string;
  /** `int32` */
  readonly rowVersion: number;
}

/** Result of `wamn-wms:product/query@1.0.0`. */
export interface ProductQueryResult {
  /** The rows this page carries. */
  readonly item: readonly ProductQueryRow[];
  /** The next page's cursor, or null at the last page. */
  readonly nextCursor: string | null;
}

/** What `wamn-wms:product/query@1.0.0` calls its result members. */
export const PRODUCT_QUERY_RESULT_FIELDS: FieldMap = {
  "item": {
    member: "item",
    fields: {
      "created_at": "createdAt",
      "id": "id",
      "product_code": "productCode",
      "row_version": "rowVersion",
    },
  },
  "next_cursor": "nextCursor",
};

/**
 * Where the release publishes `wamn-wms:product/query@1.0.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const PRODUCT_QUERY_ROUTE: OperationRoute = {
  operation: "wamn-wms:product/query@1.0.0",
  method: "POST",
  template: "/product/query",
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

/** Invoke `wamn-wms:product/query@1.0.0` through a transport the application supplies. */
export async function query(
  transport: Transport,
  items: readonly ProductQueryRequest[],
): Promise<Outcome<ProductQueryResult>> {
  return reviveOutcome<ProductQueryResult>(
    await transport.invoke({
      ...PRODUCT_QUERY_ROUTE,
      items: items.map((item) => toWire(item, PRODUCT_QUERY_REQUEST_FIELDS)),
    }),
    PRODUCT_QUERY_RESULT_FIELDS,
  );
}

/** Input for `wamn-wms:product/update@1.0.0`. */
export interface ProductUpdateRequest {
  /** `object` */
  change: ProductUpdateRequestChange;
  /** `int32` */
  expectedRowVersion: number;
  /** `uuid` */
  id: Uuid;
  /** `string` */
  requestId: string;
}

export interface ProductUpdateRequestChange {
  /** `text`, omittable */
  productCode?: string;
}

/** What `wamn-wms:product/update@1.0.0` calls its input members. */
export const PRODUCT_UPDATE_REQUEST_FIELDS: FieldMap = {
  "change": {
    member: "change",
    fields: {
      "product_code": "productCode",
    },
  },
  "expected_row_version": "expectedRowVersion",
  "id": "id",
  "request_id": "requestId",
};

/** Result of `wamn-wms:product/update@1.0.0`. */
export interface ProductUpdateResult {
  /** `timestamptz` */
  readonly createdAt: Timestamptz;
  /** `uuid` */
  readonly id: Uuid;
  /** `text` */
  readonly productCode: string;
  /** `int32` */
  readonly rowVersion: number;
}

/** What `wamn-wms:product/update@1.0.0` calls its result members. */
export const PRODUCT_UPDATE_RESULT_FIELDS: FieldMap = {
  "created_at": "createdAt",
  "id": "id",
  "product_code": "productCode",
  "row_version": "rowVersion",
};

/**
 * Where the release publishes `wamn-wms:product/update@1.0.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const PRODUCT_UPDATE_ROUTE: OperationRoute = {
  operation: "wamn-wms:product/update@1.0.0",
  method: "POST",
  template: "/product/update",
  freshOnly: false,
  contract: {
    resultClass: "one",
    partialSchema: null,
    errors: [
      { literal: "concurrency_conflict", required: ["expected_row_version", "observed_row_version"], sources: [] },
      { literal: "internal_error", required: [], sources: ["query_error", "row_limit_exceeded"] },
      { literal: "invalid_input", required: ["field"], sources: [] },
      { literal: "not_found", required: ["field", "id"], sources: [] },
      { literal: "permission_denied", required: ["operation"], sources: ["permission_denied"] },
      { literal: "retry", required: [], sources: ["connection_unavailable", "serialization_failure"] },
      { literal: "timeout", required: [], sources: ["statement_timeout"] },
      { literal: "unique_violation", required: ["constraint", "field"], sources: ["unique_violation"] },
    ],
    replay: null,
    direct: true,
    kind: "update",
    transaction: "implicit",
  },
};

/** Invoke `wamn-wms:product/update@1.0.0` through a transport the application supplies. */
export async function update(
  transport: Transport,
  items: readonly ProductUpdateRequest[],
): Promise<Outcome<ProductUpdateResult>> {
  return reviveOutcome<ProductUpdateResult>(
    await transport.invoke({
      ...PRODUCT_UPDATE_ROUTE,
      items: items.map((item) => toWire(item, PRODUCT_UPDATE_REQUEST_FIELDS)),
    }),
    PRODUCT_UPDATE_RESULT_FIELDS,
  );
}
