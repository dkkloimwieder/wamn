// @generated from the client-contract IR; do not edit.
//
// `receipt` components. Each one calls the bindings and the runtime, and
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
  announceOutcome,
} from "@wamn/ui";
import {
  RECEIPT_QUERY_REQUEST_FIELDS,
  RECEIPT_QUERY_RESULT_FIELDS,
  RECEIPT_QUERY_ROUTE,
  get,
  type ReceiptGetRequest,
  type ReceiptGetResult,
  type ReceiptQueryRequest,
  type ReceiptQueryResult,
  type ReceiptQueryRow,
} from "../receipt.js";
import {
  PURCHASE_ORDER_GET_REQUEST_FIELDS,
  PURCHASE_ORDER_GET_RESULT_FIELDS,
  PURCHASE_ORDER_GET_ROUTE,
} from "../purchase_order.js";

/** The record that the detail for `wamn-receiving:receipt/get@1.0.0` reads. */
export interface ReceiptGetDetailInput {
  readonly id: Uuid;
}

/** What the detail screen for `wamn-receiving:receipt/get@1.0.0` takes. */
export interface ReceiptGetDetailProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** The input that names the record. */
  readonly input: ReceiptGetDetailInput;
  /** Called with every outcome this screen reads. */
  readonly onOutcome?: (outcome: Outcome<ReceiptGetResult>) => void;
}

/** What an operator calls this screen. The page decides where it goes. */
export const ReceiptGetDetailLabel = "Receipt";

/**
 * The detail screen for `wamn-receiving:receipt/get@1.0.0`.
 *
 * It reads when it mounts and again whenever its input changes, because the
 * input names the record it shows.
 */
export function ReceiptGetDetail(props: ReceiptGetDetailProps) {
  const [outcome, { refetch: readAgain }] = createResource(
    () => props.input,
    async (input: ReceiptGetDetailInput) => {
      const read = await get(props.transport, [
        input as ReceiptGetRequest,
      ]);
      props.onOutcome?.(read);
      if (read.status !== "completed") {
        announceOutcome(read, ReceiptGetDetailLabel);
      }
      return read;
    },
  );
  onCleanup(afterWrites(props.transport, () => void readAgain()));
  const record = (): ReceiptGetResult | undefined => {
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
        <DetailItem term="Recorded">{cellText(readMember(record(), ["createdAt"]), "timestamptz")}</DetailItem>
        <DetailItem term="Recorded by">{cellText(readMember(record(), ["createdBy"]), "uuid")}</DetailItem>
        <DetailItem term="id">{cellText(readMember(record(), ["id"]), "uuid")}</DetailItem>
        <DetailItem term="idempotency key">{cellText(readMember(record(), ["idempotencyKey"]), "text")}</DetailItem>
        <DetailItem term="Received at">{cellText(readMember(record(), ["occurredAt"]), "timestamptz")}</DetailItem>
        <DetailItem term="Purchase order">{cellText(readMember(record(), ["purchaseOrderId"]), "uuid")}</DetailItem>
        <DetailItem term="Receipt reference">{cellText(readMember(record(), ["receiptReference"]), "text")}</DetailItem>
      </DetailList>
    </section>
  );
}

/** What the table for `wamn-receiving:receipt/query@1.0.0` takes. */
export interface ReceiptQueryTableProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** Input the parent fixes, which the operator does not edit. */
  readonly fixed?: Partial<ReceiptQueryRequest>;
  /** For each operation whose record a row opens, what opening it does. A row shows a button only for these. */
  readonly onOpen?: { readonly [operation: string]: (row: ReceiptQueryRow) => void };
  /** For each operation whose form a row fills, what the filled values do. */
  readonly onFill?: { readonly [operation: string]: (initial: object) => void };
  /** Called with every outcome this screen reads. */
  readonly onOutcome?: (outcome: Outcome<ReceiptQueryResult>) => void;
}

/** What an operator calls this screen. The page decides where it goes. */
export const ReceiptQueryTableLabel = "Receipts";

/** The table for `wamn-receiving:receipt/query@1.0.0`: the QueryTable over `RECEIPT_QUERY_TABLE`. */
export function ReceiptQueryTable(props: ReceiptQueryTableProps) {
  return <QueryTable<ReceiptQueryRow, ReceiptQueryResult> definition={RECEIPT_QUERY_TABLE} label={ReceiptQueryTableLabel} {...props} />;
}

/** The table definition of `wamn-receiving:receipt/query@1.0.0`. */
export const RECEIPT_QUERY_TABLE = {
  name: "receipt",
  read: { route: RECEIPT_QUERY_ROUTE, request: RECEIPT_QUERY_REQUEST_FIELDS, result: RECEIPT_QUERY_RESULT_FIELDS },
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
    { field: "createdAt", label: "Recorded", type: "timestamptz", role: "value" },
    { field: "createdBy", label: "Recorded by", type: "uuid", role: "value" },
    { field: "id", label: "id", type: "uuid", role: "key" },
    { field: "idempotencyKey", label: "idempotency key", type: "text", role: "value" },
    { field: "occurredAt", label: "Received at", type: "timestamptz", role: "value" },
    { field: "purchaseOrderId", label: "Purchase order", type: "uuid", role: "reference", displayField: "purchaseOrderNumber", recordRead: { read: { route: PURCHASE_ORDER_GET_ROUTE, request: PURCHASE_ORDER_GET_REQUEST_FIELDS, result: PURCHASE_ORDER_GET_RESULT_FIELDS }, keyInput: ["id"] } },
    { field: "receiptReference", label: "Receipt reference", type: "text", role: "value" },
  ],
  actions: [
    { operation: "wamn-receiving:receipt/get@1.0.0", label: "get", many: false, opens: "record", fill: [] },
  ],
  childTables: [],
} as const;
