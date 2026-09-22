// @generated from the client-contract IR; do not edit.
//
// `receipt` components. Each one calls the bindings and the runtime, and
// nothing else.

import { For, Show, createResource, createSignal } from "solid-js";
import {
  createSolidTable,
  flexRender,
  getCoreRowModel,
  type ColumnDef,
} from "@tanstack/solid-table";
import {
  appendPage,
  cellText,
  emptyPage,
  firstPage,
  hasNextPage,
  newRequestId,
  readMember,
  startRead,
  type JsonValue,
  type Outcome,
  type PageState,
  type Transport,
  type Uuid,
  writeMember,
} from "@wamn/web-runtime";
import {
  get,
  query,
  type ReceiptGetRequest,
  type ReceiptGetResult,
  type ReceiptQueryRequest,
  type ReceiptQueryResult,
  type ReceiptQueryRow,
} from "../receipt.js";

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
export const ReceiptGetDetailLabel = "get";

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
        { ...input, requestId: newRequestId() } as ReceiptGetRequest,
      ]);
      props.onOutcome?.(read);
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
        <p>{state()}</p>
      </Show>
      <dl>
        <dt>created at</dt>
        <dd>{cellText(readMember(record(), ["createdAt"]), "timestamptz")}</dd>
        <dt>created by</dt>
        <dd>{cellText(readMember(record(), ["createdBy"]), "uuid")}</dd>
        <dt>id</dt>
        <dd>{cellText(readMember(record(), ["id"]), "uuid")}</dd>
        <dt>idempotency key</dt>
        <dd>{cellText(readMember(record(), ["idempotencyKey"]), "text")}</dd>
        <dt>occurred at</dt>
        <dd>{cellText(readMember(record(), ["occurredAt"]), "timestamptz")}</dd>
        <dt>purchase order id</dt>
        <dd>{cellText(readMember(record(), ["purchaseOrderId"]), "uuid")}</dd>
        <dt>receipt reference</dt>
        <dd>{cellText(readMember(record(), ["receiptReference"]), "text")}</dd>
      </dl>
    </section>
  );
}

/** Columns of `wamn-receiving:receipt/query@1.0.0`, in contract order. */
const QUERY_COLUMNS: ColumnDef<ReceiptQueryRow, unknown>[] = [
  {
    accessorKey: "createdAt",
    header: "created at",
    cell: (cell) => cellText(cell.getValue() as JsonValue, "timestamptz"),
  },
  {
    accessorKey: "createdBy",
    header: "created by",
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
    header: "occurred at",
    cell: (cell) => cellText(cell.getValue() as JsonValue, "timestamptz"),
  },
  {
    accessorKey: "purchaseOrderId",
    header: "purchase order id",
    cell: (cell) => cellText(cell.getValue() as JsonValue, "uuid"),
  },
  {
    accessorKey: "receiptReference",
    header: "receipt reference",
    cell: (cell) => cellText(cell.getValue() as JsonValue, "text"),
  },
];

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
export const ReceiptQueryTableLabel = "query";

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
      requestId: newRequestId(),
    } as ReceiptQueryRequest;
    const sent = cursor === null ? request : (writeMember(request, ["cursor"], cursor) as ReceiptQueryRequest);
    const outcome = await query(props.transport, [sent]);
    props.onOutcome?.(outcome);
    if (outcome.status !== "completed") {
      setPage({ ...page(), busy: false });
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
    setControls((current) => writeMember(current, path, value));
    restart();
  };

  const table = createSolidTable({
    get data() {
      return page().rows as ReceiptQueryRow[];
    },
    columns: QUERY_COLUMNS,
    getCoreRowModel: getCoreRowModel(),
  });

  return (
    <section>
      <form
        onSubmit={(event) => {
          event.preventDefault();
          restart();
        }}
      >
        <label>
          limit
          <input
            type="number"
            min={1}
            max={100}
            value={100}
            onChange={(event) => change(["limit"], event.currentTarget.value)}
          />
        </label>
        <button type="submit">read</button>
      </form>
      <table>
        <thead>
          <For each={table.getHeaderGroups()}>
            {(group) => (
              <tr>
                <For each={group.headers}>
                  {(header) => (
                    <th>{flexRender(header.column.columnDef.header, header.getContext())}</th>
                  )}
                </For>
              </tr>
            )}
          </For>
        </thead>
        <tbody>
          <For each={table.getRowModel().rows}>
            {(row) => (
              <tr onClick={() => props.onRowSelect?.(row.original)}>
                <For each={row.getVisibleCells()}>
                  {(cell) => <td>{flexRender(cell.column.columnDef.cell, cell.getContext())}</td>}
                </For>
                <td>
                  <Show when={props.onOpenReceiptGet}>
                    <button type="button" onClick={() => props.onOpenReceiptGet?.(row.original)}>
                      get
                    </button>
                  </Show>
                </td>
              </tr>
            )}
          </For>
        </tbody>
      </table>
      <Show when={hasNextPage(page())}>
        <button type="button" onClick={() => void read(page().cursor)}>
          next page
        </button>
      </Show>
    </section>
  );
}
