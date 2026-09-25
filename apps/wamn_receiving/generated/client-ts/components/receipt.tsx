// @generated from the client-contract IR; do not edit.
//
// `receipt` components. Each one calls the bindings and the runtime, and
// nothing else.

import { Show, createResource, createSignal } from "solid-js";
import { createTable, type ColumnDef } from "@tanstack/solid-table";
import {
  appendPage,
  cellText,
  emptyPage,
  failedRead,
  firstPage,
  hasNextPage,
  readMember,
  startRead,
  type JsonValue,
  type Outcome,
  type PageState,
  type Transport,
  type Uuid,
  writeControl,
  writeMember,
} from "@wamn/web-runtime";
import {
  Button,
  DataGrid,
  DataGridContainer,
  DetailItem,
  DetailList,
  FieldError,
  FieldGroup,
  FormActions,
  TableScreen,
  TextField,
  WindowedTable,
  announceOutcome,
  createRecordLabels,
  gridFeatures,
  type GridFeatures,
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
  const [outcome] = createResource(
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
  /** Called when the operator picks one row. */
  readonly onRowSelect?: (row: ReceiptQueryRow) => void;
  /** Called when the operator opens `wamn-receiving:receipt/get@1.0.0` from one row. */
  readonly onOpenReceiptGet?: (row: ReceiptQueryRow) => void;
  /** Called with every outcome this screen reads. */
  readonly onOutcome?: (outcome: Outcome<ReceiptQueryResult>) => void;
}

/** What an operator calls this screen. The page decides where it goes. */
export const ReceiptQueryTableLabel = "Receipts";

/**
 * The table for `wamn-receiving:receipt/query@1.0.0`.
 *
 * It owns its page controls and its rows. A change to a control clears the
 * rows, because a cursor names a position in the list the old input produced.
 *
 * It reads when the operator asks, and not when it mounts, because a read is
 * a request that the operator did not send yet.
 */
export function ReceiptQueryTable(props: ReceiptQueryTableProps) {
  const [controls, setControls] = createSignal<Partial<ReceiptQueryRequest>>({});
  const [page, setPage] = createSignal<PageState<ReceiptQueryRow>>(emptyPage<ReceiptQueryRow>());

  const read = async (cursor: string | null) => {
    setPage(startRead(page()));
    const request = {
      ...controls(),
      ...props.fixed,
    } as ReceiptQueryRequest;
    const sent = cursor === null ? request : (writeMember(request, ["cursor"], cursor) as ReceiptQueryRequest);
    const outcome = await query(props.transport, [sent]);
    props.onOutcome?.(outcome);
    if (outcome.status !== "completed") {
      announceOutcome(outcome, ReceiptQueryTableLabel);
      setPage(failedRead(page(), outcome));
      return;
    }
    const rows = outcome.value.item;
    setPage(cursor === null ? firstPage(rows, outcome.value.nextCursor) : appendPage(page(), rows, outcome.value.nextCursor));
  };

  const restart = () => {
    setPage(emptyPage<ReceiptQueryRow>());
    void read(null);
  };

  const change = (path: readonly string[], value: JsonValue) => {
    setControls((current) => writeControl(current, path, value));
    restart();
  };

  const purchaseOrderGetLabels = createRecordLabels(async (key) => {
    const request = writeMember({}, ["id"], key) as PurchaseOrderGetRequest;
    const outcome = await purchaseOrderGet(props.transport, [request]);
    if (outcome.status !== "completed") {
      return null;
    }
    const text = outcome.value.purchaseOrderNumber;
    return text == null ? null : String(text);
  });

  const columns: ColumnDef<GridFeatures, ReceiptQueryRow>[] = [
    {
      accessorKey: "createdAt",
      header: "Recorded",
      cell: (cell) => cellText(cell.getValue() as JsonValue, "timestamptz"),
    },
    {
      accessorKey: "createdBy",
      header: "Recorded by",
      cell: (cell) => cellText(cell.getValue() as JsonValue, "uuid"),
    },
    {
      accessorKey: "id",
      header: "id",
      cell: (cell) => cellText(cell.getValue() as JsonValue, "uuid"),
    },
    {
      accessorKey: "idempotencyKey",
      header: "idempotency key",
      cell: (cell) => cellText(cell.getValue() as JsonValue, "text"),
    },
    {
      accessorKey: "occurredAt",
      header: "Received at",
      cell: (cell) => cellText(cell.getValue() as JsonValue, "timestamptz"),
    },
    {
      accessorKey: "purchaseOrderId",
      header: "Purchase order",
      cell: (cell) => <>{purchaseOrderGetLabels(cell.getValue() as string | null)}</>,
    },
    {
      accessorKey: "receiptReference",
      header: "Receipt reference",
      cell: (cell) => cellText(cell.getValue() as JsonValue, "text"),
    },
    {
      id: "openReceiptGet",
      header: "",
      cell: (cell) => (
        <Show when={props.onOpenReceiptGet}>
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => props.onOpenReceiptGet?.(cell.row.original)}
          >
            get
          </Button>
        </Show>
      ),
    },
  ];

  const table = createTable({
    features: gridFeatures,
    get data() {
      return page().rows as ReceiptQueryRow[];
    },
    columns: columns,
    manualPagination: true,
  });

  return (
    <TableScreen>
      <form
        onSubmit={(event) => {
          event.preventDefault();
          restart();
        }}
      >
        <FieldGroup>
          <TextField
            label="limit"
            type="number"
            min={1}
            max={100}
            value="100"
            onChange={(value) => change(["limit"], value)}
          />
        </FieldGroup>
        <FormActions>
          <Button type="submit">read</Button>
        </FormActions>
      </form>
      <DataGrid
        table={table}
        recordCount={page().rows.length}
        isLoading={page().busy && page().rows.length === 0}
        emptyMessage={page().refusal}
        onRowClick={(row) => props.onRowSelect?.(row)}
      >
        <DataGridContainer>
          <WindowedTable />
        </DataGridContainer>
      </DataGrid>
      <FormActions>
        <Button
          type="button"
          variant="outline"
          disabled={!hasNextPage(page())}
          onClick={() => void read(page().cursor)}
        >
          next page
        </Button>
      </FormActions>
    </TableScreen>
  );
}
