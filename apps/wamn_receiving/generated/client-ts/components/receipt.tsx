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
  writeMember,
} from "@wamn/web-runtime";
import {
  Button,
  DataTable,
  DetailItem,
  DetailList,
  FieldError,
  TableScreen,
  announceOutcome,
  createRecordLabels,
  createTableLoad,
} from "@wamn/ui";
import {
  get,
  query,
  type ReceiptGetRequest,
  type ReceiptGetResult,
  type ReceiptQueryRequest,
  type ReceiptQueryResult,
  type ReceiptQueryRow,
} from "../receipt.js";
import {
  get as purchaseOrderGet,
  type PurchaseOrderGetRequest,
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
  /** Called when the operator opens `wamn-receiving:receipt/get@1.0.0` from one row. */
  readonly onOpenReceiptGet?: (row: ReceiptQueryRow) => void;
  /** Called with every outcome this screen reads. */
  readonly onOutcome?: (outcome: Outcome<ReceiptQueryResult>) => void;
}

/** What an operator calls this screen. The page decides where it goes. */
export const ReceiptQueryTableLabel = "Receipts";

/**
 * The table for `wamn-receiving:receipt/query@1.0.0`: the DataTable over `RECEIPT_QUERY_TABLE`.
 *
 * It loads when it mounts. A change to a filter, a sort of rows the load did
 * not read in full, a cap change and a refresh each start a new load.
 */
export function ReceiptQueryTable(props: ReceiptQueryTableProps) {
  const load = createTableLoad<ReceiptQueryRow>(RECEIPT_QUERY_TABLE, async (limit) => {
    let request = { ...props.fixed } as ReceiptQueryRequest;
    request = writeMember(request, ["limit"], limit) as ReceiptQueryRequest;
    const outcome = await query(props.transport, [request]);
    props.onOutcome?.(outcome);
    if (outcome.status !== "completed") {
      announceOutcome(outcome, ReceiptQueryTableLabel);
    }
    return outcome;
  });
  void load.load();
  onCleanup(afterWrites(props.transport, () => void load.load()));

  const purchaseOrderGetLabels = createRecordLabels(async (key) => {
    const request = writeMember({}, ["id"], key) as PurchaseOrderGetRequest;
    const outcome = await purchaseOrderGet(props.transport, [request]);
    if (outcome.status !== "completed") {
      return null;
    }
    const text = outcome.value.purchaseOrderNumber;
    return text == null ? null : String(text);
  });

  const columns = RECEIPT_QUERY_TABLE.columns.map((column) => {
    switch (column.field) {
      case "purchaseOrderId":
        return { ...column, cell: (value: unknown) => <>{purchaseOrderGetLabels(value as string | null)}</> };
      default:
        return column;
    }
  });

  const actions = (row: ReceiptQueryRow) => (
    <>
      <Show when={props.onOpenReceiptGet}>
        <Button type="button" variant="outline" size="sm" onClick={() => props.onOpenReceiptGet?.(row)}>
          get
        </Button>
      </Show>
    </>
  );

  return (
    <TableScreen>
      <DataTable
        name="receipt"
        columns={columns}
        rowId={RECEIPT_QUERY_TABLE.rowId}
        rows={load.state().rows}
        fullyRead={load.state().fullyRead}
        busy={load.state().busy}
        refusal={load.state().refusal}
        cap={load.state().cap}
        onCapChange={(cap) => void load.load(cap)}
        onRefresh={() => void load.load()}
        startedAt={load.state().startedAt}
        endedAt={load.state().endedAt}
        sortFields={RECEIPT_QUERY_TABLE.sortFields}
        sortMaxFields={RECEIPT_QUERY_TABLE.sortMaxFields}
        onSortChange={load.sortBy}
        scopeFilters={RECEIPT_QUERY_TABLE.scopeFilters}
        onScopeChange={() => void load.load()}
        rowActions={actions}
      />
    </TableScreen>
  );
}

/** The table definition of `wamn-receiving:receipt/query@1.0.0`. */
export const RECEIPT_QUERY_TABLE = {
  read: "query",
  rowId: "id",
  pageMaximum: 100,
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
    { field: "purchaseOrderId", label: "Purchase order", type: "uuid", role: "reference", displayField: "purchaseOrderNumber" },
    { field: "receiptReference", label: "Receipt reference", type: "text", role: "value" },
  ],
} as const;
