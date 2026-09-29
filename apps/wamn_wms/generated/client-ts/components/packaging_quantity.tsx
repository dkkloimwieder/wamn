// @generated from the client-contract IR; do not edit.
//
// `packaging_quantity` components. Each one calls the bindings and the runtime, and
// nothing else.

import { Show, createResource, onCleanup } from "solid-js";
import {
  afterWrites,
  cellText,
  readMember,
  type Outcome,
  type Transport,
  type Uuid,
} from "@wamn/web-runtime";
import {
  DetailItem,
  DetailList,
  FieldError,
  QueryTable,
  TableScreen,
  announceOutcome,
} from "@wamn/ui";
import {
  PACKAGING_QUANTITY_QUERY_REQUEST_FIELDS,
  PACKAGING_QUANTITY_QUERY_RESULT_FIELDS,
  PACKAGING_QUANTITY_QUERY_ROUTE,
  get,
  type PackagingQuantityGetRequest,
  type PackagingQuantityGetResult,
  type PackagingQuantityQueryRequest,
  type PackagingQuantityQueryResult,
  type PackagingQuantityQueryRow,
} from "../packaging_quantity.js";
import {
  PackagingQuantityGetDetailLabel,
  PackagingQuantityQueryTableLabel,
} from "./labels.js";
import {
  PACKAGING_GET_REQUEST_FIELDS,
  PACKAGING_GET_RESULT_FIELDS,
  PACKAGING_GET_ROUTE,
} from "../packaging.js";
import {
  PRODUCT_GET_REQUEST_FIELDS,
  PRODUCT_GET_RESULT_FIELDS,
  PRODUCT_GET_ROUTE,
} from "../product.js";

/** The record that the detail for `wamn-wms:packaging-quantity/get@1.0.0` reads. */
export interface PackagingQuantityGetDetailInput {
  readonly id: Uuid;
}

/** What the detail screen for `wamn-wms:packaging-quantity/get@1.0.0` takes. */
export interface PackagingQuantityGetDetailProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** The input that names the record. */
  readonly input: PackagingQuantityGetDetailInput;
  /** Called with every outcome this screen reads. */
  readonly onOutcome?: (outcome: Outcome<PackagingQuantityGetResult>) => void;
}

/**
 * The detail screen for `wamn-wms:packaging-quantity/get@1.0.0`.
 *
 * It reads when it mounts and again whenever its input changes, because the
 * input names the record it shows.
 */
export function PackagingQuantityGetDetail(props: PackagingQuantityGetDetailProps) {
  const [outcome, { refetch: readAgain }] = createResource(
    () => props.input,
    async (input: PackagingQuantityGetDetailInput) => {
      const read = await get(props.transport, [
        input as PackagingQuantityGetRequest,
      ]);
      props.onOutcome?.(read);
      if (read.status !== "completed") {
        announceOutcome(read, PackagingQuantityGetDetailLabel);
      }
      return read;
    },
  );
  onCleanup(afterWrites(props.transport, () => void readAgain()));
  const record = (): PackagingQuantityGetResult | undefined => {
    const read = outcome();
    return read?.status === "completed" ? read.value : undefined;
  };
  const state = () => outcome()?.status;

  return (
    <section>
      <Show when={state() !== undefined && state() !== "completed"}>
        <FieldError>{state()}</FieldError>
      </Show>
      <DetailList loading={outcome.loading}>
        <DetailItem term="created at">{cellText(readMember(record(), ["createdAt"]), "timestamptz")}</DetailItem>
        <DetailItem term="id">{cellText(readMember(record(), ["id"]), "uuid")}</DetailItem>
        <DetailItem term="packaging id">{cellText(readMember(record(), ["packagingId"]), "uuid")}</DetailItem>
        <DetailItem term="product id">{cellText(readMember(record(), ["productId"]), "uuid")}</DetailItem>
        <DetailItem term="quantity">{cellText(readMember(record(), ["quantity"]), "numeric")}</DetailItem>
        <DetailItem term="status">{cellText(readMember(record(), ["status"]), "text")}</DetailItem>
      </DetailList>
    </section>
  );
}

/** What the table for `wamn-wms:packaging-quantity/query@1.0.0` takes. */
export interface PackagingQuantityQueryTableProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** Input the parent fixes, which the operator does not edit. */
  readonly fixed?: Partial<PackagingQuantityQueryRequest>;
  /** For each operation whose record a row opens, what opening it does. A row shows a button only for these. */
  readonly onOpen?: { readonly [operation: string]: (row: PackagingQuantityQueryRow) => void };
  /** For each operation whose form a row fills, what the filled values do. */
  readonly onFill?: { readonly [operation: string]: (initial: object) => void };
  /** Called with every outcome this screen reads. */
  readonly onOutcome?: (outcome: Outcome<PackagingQuantityQueryResult>) => void;
}

/** The table for `wamn-wms:packaging-quantity/query@1.0.0`: the QueryTable over `PACKAGING_QUANTITY_QUERY_TABLE`, in the table screen. */
export function PackagingQuantityQueryTable(props: PackagingQuantityQueryTableProps) {
  return (
    <TableScreen>
      <QueryTable<PackagingQuantityQueryRow, PackagingQuantityQueryResult> definition={PACKAGING_QUANTITY_QUERY_TABLE} label={PackagingQuantityQueryTableLabel} {...props} />
    </TableScreen>
  );
}

/** The table definition of `wamn-wms:packaging-quantity/query@1.0.0`. */
export const PACKAGING_QUANTITY_QUERY_TABLE = {
  name: "packaging-quantity",
  read: { route: PACKAGING_QUANTITY_QUERY_ROUTE, request: PACKAGING_QUANTITY_QUERY_REQUEST_FIELDS, result: PACKAGING_QUANTITY_QUERY_RESULT_FIELDS },
  rows: "item",
  rowId: ["id"],
  pageMaximum: 100,
  limitInput: ["limit"],
  sortFieldInput: null,
  sortDirectionInput: null,
  filters: [],
  scopeFilters: [],
  sortFields: [],
  sortDirections: [],
  sortMaxFields: 1,
  columns: [
    { field: "createdAt", label: "created at", type: "timestamptz", role: "value" },
    { field: "id", label: "id", type: "uuid", role: "key" },
    { field: "packagingId", label: "packaging id", type: "uuid", role: "reference", displayField: "packagingCode", recordRead: { read: { route: PACKAGING_GET_ROUTE, request: PACKAGING_GET_REQUEST_FIELDS, result: PACKAGING_GET_RESULT_FIELDS }, keyInput: ["id"] } },
    { field: "productId", label: "product id", type: "uuid", role: "reference", displayField: "productCode", recordRead: { read: { route: PRODUCT_GET_ROUTE, request: PRODUCT_GET_REQUEST_FIELDS, result: PRODUCT_GET_RESULT_FIELDS }, keyInput: ["id"] } },
    { field: "quantity", label: "quantity", type: "numeric", role: "value" },
    { field: "status", label: "status", type: "text", role: "value" },
  ],
  actions: [
    { operation: "wamn-wms:packaging-quantity/get@1.0.0", reference: "wamn-wms:packaging-quantity/get", label: "get", many: false, opens: "record", fill: [] },
  ],
  childTables: [],
} as const;
