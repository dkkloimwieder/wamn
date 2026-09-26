// @generated from the client-contract IR; do not edit.
//
// `supplier` operations of package `wamn_receiving`.

import type { FieldMap, OperationRoute, Outcome, Timestamptz, Transport, Uuid } from "@wamn/web-runtime";
import { reviveOutcome, toWire } from "@wamn/web-runtime";

/** Input for `wamn-receiving:supplier/create@1.0.0`. */
export interface SupplierCreateRequest {
  /** `text` */
  idempotencyKey: string;
  /** `text` */
  name: string;
  /** `string` */
  requestId: string;
}

/** What `wamn-receiving:supplier/create@1.0.0` calls its input members. */
export const SUPPLIER_CREATE_REQUEST_FIELDS: FieldMap = {
  "idempotency_key": "idempotencyKey",
  "name": "name",
  "request_id": "requestId",
};

/** Result of `wamn-receiving:supplier/create@1.0.0`. */
export interface SupplierCreateResult {
  /** `timestamptz` */
  readonly createdAt: Timestamptz;
  /** `uuid` */
  readonly id: Uuid;
  /** `text` */
  readonly name: string;
}

/** What `wamn-receiving:supplier/create@1.0.0` calls its result members. */
export const SUPPLIER_CREATE_RESULT_FIELDS: FieldMap = {
  "created_at": "createdAt",
  "id": "id",
  "name": "name",
};

/**
 * Where the release publishes `wamn-receiving:supplier/create@1.0.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const SUPPLIER_CREATE_ROUTE: OperationRoute = {
  operation: "wamn-receiving:supplier/create@1.0.0",
  method: "POST",
  template: "/supplier/create",
  freshOnly: false,
  contract: {
    resultClass: "one",
    partialSchema: null,
    errors: [
      { literal: "idempotency_conflict", required: ["field"], sources: ["changed_canonical_command"], text: null },
      { literal: "internal_error", required: [], sources: ["query_error", "row_limit_exceeded"], text: null },
      { literal: "invalid_input", required: ["field"], sources: [], text: null },
      { literal: "permission_denied", required: ["operation"], sources: ["permission_denied"], text: null },
      { literal: "retry", required: [], sources: ["connection_unavailable", "serialization_failure"], text: null },
      { literal: "timeout", required: [], sources: ["statement_timeout"], text: null },
      { literal: "unique_violation", required: ["constraint"], sources: ["unique_violation"], text: null },
    ],
    replay: "claim",
    direct: true,
    kind: "create",
    transaction: "explicit_per_input",
  },
};

/** Invoke `wamn-receiving:supplier/create@1.0.0` through a transport the application supplies. */
export async function create(
  transport: Transport,
  items: readonly SupplierCreateRequest[],
): Promise<Outcome<SupplierCreateResult>> {
  return reviveOutcome<SupplierCreateResult>(
    await transport.invoke({
      ...SUPPLIER_CREATE_ROUTE,
      items: items.map((item) => toWire(item, SUPPLIER_CREATE_REQUEST_FIELDS)),
    }),
    SUPPLIER_CREATE_RESULT_FIELDS,
  );
}

/** Input for `wamn-receiving:supplier/query@1.0.0`. */
export interface SupplierQueryRequest {
  /** `text`, omittable */
  cursor?: string;
  /** `int32`, omittable */
  limit?: number;
}

/** What `wamn-receiving:supplier/query@1.0.0` calls its input members. */
export const SUPPLIER_QUERY_REQUEST_FIELDS: FieldMap = {
  "cursor": "cursor",
  "limit": "limit",
};

/** One row of `wamn-receiving:supplier/query@1.0.0`. */
export interface SupplierQueryRow {
  /** `timestamptz` */
  readonly createdAt: Timestamptz;
  /** `uuid` */
  readonly id: Uuid;
  /** `text` */
  readonly name: string;
}

/** Result of `wamn-receiving:supplier/query@1.0.0`. */
export interface SupplierQueryResult {
  /** The rows this page carries. */
  readonly item: readonly SupplierQueryRow[];
  /** The next page's cursor, or null at the last page. */
  readonly nextCursor: string | null;
}

/** What `wamn-receiving:supplier/query@1.0.0` calls its result members. */
export const SUPPLIER_QUERY_RESULT_FIELDS: FieldMap = {
  "item": {
    member: "item",
    fields: {
      "created_at": "createdAt",
      "id": "id",
      "name": "name",
    },
  },
  "next_cursor": "nextCursor",
};

/**
 * Where the release publishes `wamn-receiving:supplier/query@1.0.0`.
 *
 * Method and template only. The host and base URL are the application's
 * deployment configuration, not this release's facts.
 */
export const SUPPLIER_QUERY_ROUTE: OperationRoute = {
  operation: "wamn-receiving:supplier/query@1.0.0",
  method: "GET",
  template: "/supplier/query",
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

/** Invoke `wamn-receiving:supplier/query@1.0.0` through a transport the application supplies. */
export async function query(
  transport: Transport,
  items: readonly SupplierQueryRequest[],
): Promise<Outcome<SupplierQueryResult>> {
  return reviveOutcome<SupplierQueryResult>(
    await transport.invoke({
      ...SUPPLIER_QUERY_ROUTE,
      items: items.map((item) => toWire(item, SUPPLIER_QUERY_REQUEST_FIELDS)),
    }),
    SUPPLIER_QUERY_RESULT_FIELDS,
  );
}
