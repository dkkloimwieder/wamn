// @generated from the client-contract IR; do not edit.
//
// `inventory_transaction` components. Each one calls the bindings and the runtime, and
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
  INVENTORY_TRANSACTION_QUERY_REQUEST_FIELDS,
  INVENTORY_TRANSACTION_QUERY_RESULT_FIELDS,
  INVENTORY_TRANSACTION_QUERY_ROUTE,
  get,
  type InventoryTransactionGetRequest,
  type InventoryTransactionGetResult,
  type InventoryTransactionQueryRequest,
  type InventoryTransactionQueryResult,
  type InventoryTransactionQueryRow,
} from "../inventory_transaction.js";
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

/** The record that the detail for `wamn-wms:inventory-transaction/get@1.0.0` reads. */
export interface InventoryTransactionGetDetailInput {
  readonly id: Uuid;
}

/** What the detail screen for `wamn-wms:inventory-transaction/get@1.0.0` takes. */
export interface InventoryTransactionGetDetailProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** The input that names the record. */
  readonly input: InventoryTransactionGetDetailInput;
  /** Called with every outcome this screen reads. */
  readonly onOutcome?: (outcome: Outcome<InventoryTransactionGetResult>) => void;
}

/** What an operator calls this screen. The page decides where it goes. */
export const InventoryTransactionGetDetailLabel = "get";

/**
 * The detail screen for `wamn-wms:inventory-transaction/get@1.0.0`.
 *
 * It reads when it mounts and again whenever its input changes, because the
 * input names the record it shows.
 */
export function InventoryTransactionGetDetail(props: InventoryTransactionGetDetailProps) {
  const [outcome, { refetch: readAgain }] = createResource(
    () => props.input,
    async (input: InventoryTransactionGetDetailInput) => {
      const read = await get(props.transport, [
        input as InventoryTransactionGetRequest,
      ]);
      props.onOutcome?.(read);
      if (read.status !== "completed") {
        announceOutcome(read, InventoryTransactionGetDetailLabel);
      }
      return read;
    },
  );
  onCleanup(afterWrites(props.transport, () => void readAgain()));
  const record = (): InventoryTransactionGetResult | undefined => {
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
        <DetailItem term="created by">{cellText(readMember(record(), ["createdBy"]), "uuid")}</DetailItem>
        <DetailItem term="from packaging id">{cellText(readMember(record(), ["fromPackagingId"]), "uuid")}</DetailItem>
        <DetailItem term="from status">{cellText(readMember(record(), ["fromStatus"]), "text")}</DetailItem>
        <DetailItem term="id">{cellText(readMember(record(), ["id"]), "uuid")}</DetailItem>
        <DetailItem term="occurred at">{cellText(readMember(record(), ["occurredAt"]), "timestamptz")}</DetailItem>
        <DetailItem term="product id">{cellText(readMember(record(), ["productId"]), "uuid")}</DetailItem>
        <DetailItem term="quantity">{cellText(readMember(record(), ["quantity"]), "numeric")}</DetailItem>
        <DetailItem term="reason code">{cellText(readMember(record(), ["reasonCode"]), "text")}</DetailItem>
        <DetailItem term="to packaging id">{cellText(readMember(record(), ["toPackagingId"]), "uuid")}</DetailItem>
        <DetailItem term="to status">{cellText(readMember(record(), ["toStatus"]), "text")}</DetailItem>
      </DetailList>
    </section>
  );
}

/** What the table for `wamn-wms:inventory-transaction/query@1.0.0` takes. */
export interface InventoryTransactionQueryTableProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** Input the parent fixes, which the operator does not edit. */
  readonly fixed?: Partial<InventoryTransactionQueryRequest>;
  /** For each operation whose record a row opens, what opening it does. A row shows a button only for these. */
  readonly onOpen?: { readonly [operation: string]: (row: InventoryTransactionQueryRow) => void };
  /** For each operation whose form a row fills, what the filled values do. */
  readonly onFill?: { readonly [operation: string]: (initial: object) => void };
  /** Called with every outcome this screen reads. */
  readonly onOutcome?: (outcome: Outcome<InventoryTransactionQueryResult>) => void;
}

/** What an operator calls this screen. The page decides where it goes. */
export const InventoryTransactionQueryTableLabel = "query";

/** The table for `wamn-wms:inventory-transaction/query@1.0.0`: the QueryTable over `INVENTORY_TRANSACTION_QUERY_TABLE`, in the table screen. */
export function InventoryTransactionQueryTable(props: InventoryTransactionQueryTableProps) {
  return (
    <TableScreen>
      <QueryTable<InventoryTransactionQueryRow, InventoryTransactionQueryResult> definition={INVENTORY_TRANSACTION_QUERY_TABLE} label={InventoryTransactionQueryTableLabel} {...props} />
    </TableScreen>
  );
}

/** The table definition of `wamn-wms:inventory-transaction/query@1.0.0`. */
export const INVENTORY_TRANSACTION_QUERY_TABLE = {
  name: "inventory-transaction",
  read: { route: INVENTORY_TRANSACTION_QUERY_ROUTE, request: INVENTORY_TRANSACTION_QUERY_REQUEST_FIELDS, result: INVENTORY_TRANSACTION_QUERY_RESULT_FIELDS },
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
    { field: "createdBy", label: "created by", type: "uuid", role: "value" },
    { field: "fromPackagingId", label: "from packaging id", type: "uuid", role: "reference", displayField: "packagingCode", recordRead: { read: { route: PACKAGING_GET_ROUTE, request: PACKAGING_GET_REQUEST_FIELDS, result: PACKAGING_GET_RESULT_FIELDS }, keyInput: ["id"] } },
    { field: "fromStatus", label: "from status", type: "text", role: "value" },
    { field: "id", label: "id", type: "uuid", role: "key" },
    { field: "occurredAt", label: "occurred at", type: "timestamptz", role: "value" },
    { field: "productId", label: "product id", type: "uuid", role: "reference", displayField: "productCode", recordRead: { read: { route: PRODUCT_GET_ROUTE, request: PRODUCT_GET_REQUEST_FIELDS, result: PRODUCT_GET_RESULT_FIELDS }, keyInput: ["id"] } },
    { field: "quantity", label: "quantity", type: "numeric", role: "value" },
    { field: "reasonCode", label: "reason code", type: "text", role: "value" },
    { field: "toPackagingId", label: "to packaging id", type: "uuid", role: "reference", displayField: "packagingCode", recordRead: { read: { route: PACKAGING_GET_ROUTE, request: PACKAGING_GET_REQUEST_FIELDS, result: PACKAGING_GET_RESULT_FIELDS }, keyInput: ["id"] } },
    { field: "toStatus", label: "to status", type: "text", role: "value" },
  ],
  actions: [
    { operation: "wamn-wms:inventory-transaction/get@1.0.0", label: "get", many: false, opens: "record", fill: [] },
  ],
  childTables: [],
} as const;
