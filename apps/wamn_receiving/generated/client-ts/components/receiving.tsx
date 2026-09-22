// @generated from the client-contract IR; do not edit.
//
// `receiving` components. Each one calls the bindings and the runtime, and
// nothing else.

import { For, Show, createEffect, createSignal } from "solid-js";
import {
  createSolidTable,
  flexRender,
  getCoreRowModel,
  type ColumnDef,
} from "@tanstack/solid-table";
import { createForm, useStore } from "@tanstack/solid-form";
import { z } from "zod";
import {
  appendPage,
  canAdd,
  canRemove,
  cellText,
  checkedMember,
  emptyPage,
  firstPage,
  hasNextPage,
  newIdempotencyKey,
  newRequestId,
  occurredAt,
  refusalMarks,
  refusedMember,
  startRead,
  type JsonValue,
  type Outcome,
  type PageState,
  type Transport,
  type Uuid,
  writeMember,
} from "@wamn/web-runtime";
import {
  RECEIVING_RECORD_RECEIPT_REQUEST_FIELDS,
  loadPurchaseOrderHistory,
  loadReceiptScreen,
  loadReceiptScreen as receivingLoadReceiptScreen,
  recordReceipt,
  type ReceivingLoadPurchaseOrderHistoryRequest,
  type ReceivingLoadPurchaseOrderHistoryResult,
  type ReceivingLoadPurchaseOrderHistoryRow,
  type ReceivingLoadReceiptScreenRequest,
  type ReceivingLoadReceiptScreenResult,
  type ReceivingLoadReceiptScreenRow,
  type ReceivingRecordReceiptRequest,
  type ReceivingRecordReceiptResult,
} from "../receiving.js";
import {
  list as locationList,
  type LocationListRequest,
  type LocationListRow,
} from "../location.js";
import {
  query as purchaseOrderQuery,
  type PurchaseOrderQueryRequest,
  type PurchaseOrderQueryRow,
} from "../purchase_order.js";

/** What the release accepts: one UUID, hyphenated. */
const UUID_TEXT = /^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$/;

/** What the release accepts: decimal text without an exponent. */
const NUMERIC_TEXT = /^[+-]?(\d+(\.\d*)?|\.\d+)$/;

/** Columns of `wamn-receiving:receiving/load-purchase-order-history@1.0.0`, in contract order. */
const LOAD_PURCHASE_ORDER_HISTORY_COLUMNS: ColumnDef<ReceivingLoadPurchaseOrderHistoryRow, unknown>[] = [
  {
    accessorKey: "after",
    header: "After",
    cell: (cell) => cellText(cell.getValue() as JsonValue, "text"),
  },
  {
    accessorKey: "before",
    header: "Before",
    cell: (cell) => cellText(cell.getValue() as JsonValue, "text"),
  },
  {
    accessorKey: "changedAt",
    header: "Changed at",
    cell: (cell) => cellText(cell.getValue() as JsonValue, "timestamptz"),
  },
  {
    accessorKey: "changedBy",
    header: "Changed by",
    cell: (cell) => cellText(cell.getValue() as JsonValue, "uuid"),
  },
  {
    accessorKey: "current",
    header: "Current",
    cell: (cell) => cellText(cell.getValue() as JsonValue, "text"),
  },
  {
    accessorKey: "cursor",
    header: "Position",
    cell: (cell) => cellText(cell.getValue() as JsonValue, "text"),
  },
  {
    accessorKey: "kind",
    header: "Change",
    cell: (cell) => cellText(cell.getValue() as JsonValue, "text"),
  },
  {
    accessorKey: "operation",
    header: "Operation",
    cell: (cell) => cellText(cell.getValue() as JsonValue, "text"),
  },
];

/** What the table for `wamn-receiving:receiving/load-purchase-order-history@1.0.0` takes. */
export interface ReceivingLoadPurchaseOrderHistoryTableProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** Input the parent fixes, which the operator does not edit. */
  readonly fixed?: Partial<ReceivingLoadPurchaseOrderHistoryRequest>;
  /** Called when the operator picks one row. */
  readonly onRowSelect?: (row: ReceivingLoadPurchaseOrderHistoryRow) => void;
  /** Called with every outcome this screen reads. */
  readonly onOutcome?: (outcome: Outcome<ReceivingLoadPurchaseOrderHistoryResult>) => void;
}

/** What an operator calls this screen. The page decides where it goes. */
export const ReceivingLoadPurchaseOrderHistoryTableLabel = "Purchase order history";

/**
 * The table for `wamn-receiving:receiving/load-purchase-order-history@1.0.0`.
 *
 * It owns its page controls and its rows. A change to a control clears the
 * rows, because a cursor names a position in the list the old input produced.
 *
 * It reads when the operator asks, and not when it mounts, because a read is
 * a request that the operator did not send yet.
 */
export function ReceivingLoadPurchaseOrderHistoryTable(props: ReceivingLoadPurchaseOrderHistoryTableProps) {
  const controls = (): Partial<ReceivingLoadPurchaseOrderHistoryRequest> => ({});
  const [page, setPage] = createSignal<PageState<ReceivingLoadPurchaseOrderHistoryRow>>(emptyPage<ReceivingLoadPurchaseOrderHistoryRow>());

  const read = async (cursor: string | null) => {
    setPage(startRead(page()));
    const request = {
      ...controls(),
      ...props.fixed,
      requestId: newRequestId(),
    } as ReceivingLoadPurchaseOrderHistoryRequest;
    const sent = request;
    const outcome = await loadPurchaseOrderHistory(props.transport, [sent]);
    props.onOutcome?.(outcome);
    if (outcome.status !== "completed") {
      setPage({ ...page(), busy: false });
      return;
    }
    const rows = outcome.value.rows;
    setPage(cursor === null ? firstPage(rows, null) : appendPage(page(), rows, null));
  };

  const restart = () => {
    setPage(emptyPage<ReceivingLoadPurchaseOrderHistoryRow>());
    void read(null);
  };

  const table = createSolidTable({
    get data() {
      return page().rows as ReceivingLoadPurchaseOrderHistoryRow[];
    },
    columns: LOAD_PURCHASE_ORDER_HISTORY_COLUMNS,
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

/** Columns of `wamn-receiving:receiving/load-receipt-screen@1.0.0`, in contract order. */
const LOAD_RECEIPT_SCREEN_COLUMNS: ColumnDef<ReceivingLoadReceiptScreenRow, unknown>[] = [
  {
    accessorKey: "itemId",
    header: "item id",
    cell: (cell) => cellText(cell.getValue() as JsonValue, "uuid"),
  },
  {
    accessorKey: "itemNumber",
    header: "Item",
    cell: (cell) => cellText(cell.getValue() as JsonValue, "text"),
  },
  {
    accessorKey: "lineId",
    header: "line id",
    cell: (cell) => cellText(cell.getValue() as JsonValue, "uuid"),
  },
  {
    accessorKey: "lineNumber",
    header: "Line",
    cell: (cell) => cellText(cell.getValue() as JsonValue, "int32"),
  },
  {
    accessorKey: "orderedQuantity",
    header: "Ordered",
    cell: (cell) => cellText(cell.getValue() as JsonValue, "numeric"),
  },
  {
    accessorKey: "purchaseOrderId",
    header: "purchase order id",
    cell: (cell) => cellText(cell.getValue() as JsonValue, "uuid"),
  },
  {
    accessorKey: "purchaseOrderNumber",
    header: "Order number",
    cell: (cell) => cellText(cell.getValue() as JsonValue, "text"),
  },
  {
    accessorKey: "purchaseOrderStatus",
    header: "Status",
    cell: (cell) => cellText(cell.getValue() as JsonValue, "text"),
  },
  {
    accessorKey: "receivedQuantity",
    header: "Received",
    cell: (cell) => cellText(cell.getValue() as JsonValue, "numeric"),
  },
  {
    accessorKey: "remainingQuantity",
    header: "Remaining",
    cell: (cell) => cellText(cell.getValue() as JsonValue, "numeric"),
  },
  {
    accessorKey: "rowVersion",
    header: "Revision",
    cell: (cell) => cellText(cell.getValue() as JsonValue, "int32"),
  },
  {
    accessorKey: "supplierId",
    header: "Supplier",
    cell: (cell) => cellText(cell.getValue() as JsonValue, "uuid"),
  },
];

/** What the table for `wamn-receiving:receiving/load-receipt-screen@1.0.0` takes. */
export interface ReceivingLoadReceiptScreenTableProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** Input the parent fixes, which the operator does not edit. */
  readonly fixed?: Partial<ReceivingLoadReceiptScreenRequest>;
  /** Called when the operator picks one row. */
  readonly onRowSelect?: (row: ReceivingLoadReceiptScreenRow) => void;
  /** Called with the values one row hands to `wamn-receiving:receiving/record-receipt@1.0.0`. */
  readonly onFillReceivingRecordReceipt?: (initial: ReceivingRecordReceiptFormInitial) => void;
  /** Called with every outcome this screen reads. */
  readonly onOutcome?: (outcome: Outcome<ReceivingLoadReceiptScreenResult>) => void;
}

/** What an operator calls this screen. The page decides where it goes. */
export const ReceivingLoadReceiptScreenTableLabel = "Receiving screen";

/**
 * The table for `wamn-receiving:receiving/load-receipt-screen@1.0.0`.
 *
 * It owns its page controls and its rows. A change to a control clears the
 * rows, because a cursor names a position in the list the old input produced.
 *
 * It reads when the operator asks, and not when it mounts, because a read is
 * a request that the operator did not send yet.
 */
export function ReceivingLoadReceiptScreenTable(props: ReceivingLoadReceiptScreenTableProps) {
  const controls = (): Partial<ReceivingLoadReceiptScreenRequest> => ({});
  const [page, setPage] = createSignal<PageState<ReceivingLoadReceiptScreenRow>>(emptyPage<ReceivingLoadReceiptScreenRow>());

  const read = async (cursor: string | null) => {
    setPage(startRead(page()));
    const request = {
      ...controls(),
      ...props.fixed,
      requestId: newRequestId(),
    } as ReceivingLoadReceiptScreenRequest;
    const sent = request;
    const outcome = await loadReceiptScreen(props.transport, [sent]);
    props.onOutcome?.(outcome);
    if (outcome.status !== "completed") {
      setPage({ ...page(), busy: false });
      return;
    }
    const rows = outcome.value.rows;
    setPage(cursor === null ? firstPage(rows, null) : appendPage(page(), rows, null));
  };

  const restart = () => {
    setPage(emptyPage<ReceivingLoadReceiptScreenRow>());
    void read(null);
  };

  const table = createSolidTable({
    get data() {
      return page().rows as ReceivingLoadReceiptScreenRow[];
    },
    columns: LOAD_RECEIPT_SCREEN_COLUMNS,
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
                  <Show when={props.onFillReceivingRecordReceipt}>
                    <button
                      type="button"
                      onClick={() => props.onFillReceivingRecordReceipt?.(writeMember({} as ReceivingRecordReceiptFormInitial, ["value", "line", "purchaseOrderLineId"], row.original.lineId))}
                    >
                      record-receipt
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

/** What an operator types for `wamn-receiving:receiving/record-receipt@1.0.0`. */
const RECORD_RECEIPT_INPUT = z.object({
  value: z
    .object({
      line: z
        .object({
          locationId: z.string().regex(UUID_TEXT, "expected a UUID"),
          purchaseOrderLineId: z.string().regex(UUID_TEXT, "expected a UUID"),
          quantity: z.string().regex(NUMERIC_TEXT, "expected decimal text"),
        })
        .array()
        .min(1)
        .max(100)
        .optional(),
      purchaseOrderId: z.string().regex(UUID_TEXT, "expected a UUID"),
      receiptReference: z.string(),
    })
    .optional(),
});

/** What the form for `wamn-receiving:receiving/record-receipt@1.0.0` can start with. */
export interface ReceivingRecordReceiptFormInitial {
  value?: {
    purchaseOrderId?: Uuid;
    receiptReference?: string;
  };
}

/** What the form for `wamn-receiving:receiving/record-receipt@1.0.0` takes. */
export interface ReceivingRecordReceiptFormProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** Values the form starts with. */
  readonly initial?: ReceivingRecordReceiptFormInitial;
  /** Called with the outcome of every submission. */
  readonly onSubmitted?: (outcome: Outcome<ReceivingRecordReceiptResult>) => void;
}

/** What an operator calls this screen. The page decides where it goes. */
export const ReceivingRecordReceiptFormLabel = "Record a receipt";

/**
 * The form for `wamn-receiving:receiving/record-receipt@1.0.0`.
 *
 * It renders what the operator fills and nothing else. The reserved inputs
 * come from the runtime at submit time, and the operator never sees them.
 */
export function ReceivingRecordReceiptForm(props: ReceivingRecordReceiptFormProps) {
  const [refusal, setRefusal] = createSignal<{ code: string | null; member: string | null } | null>(
    null,
  );

  const form = createForm(() => ({
    defaultValues: { ...props.initial } as Partial<ReceivingRecordReceiptRequest>,
    onSubmit: async ({ value }: { value: Partial<ReceivingRecordReceiptRequest> }) => {
      const checked = RECORD_RECEIPT_INPUT.safeParse(value);
      if (!checked.success) {
        const issue = checked.error.issues[0];
        setRefusal({
          code: issue?.message ?? "the input is not valid",
          member: checkedMember(
            issue?.path as (string | number)[] | undefined,
            RECEIVING_RECORD_RECEIPT_REQUEST_FIELDS,
          ),
        });
        return;
      }
      let item = { ...value } as ReceivingRecordReceiptRequest;
      item = writeMember(item, ["requestId"], newRequestId());
      item = writeMember(item, ["value", "idempotencyKey"], newIdempotencyKey());
      item = writeMember(item, ["value", "occurredAt"], occurredAt());
      const outcome = await recordReceipt(props.transport, [item]);
      props.onSubmitted?.(outcome);
      setRefusal(
        outcome.status === "refused"
          ? { code: outcome.code, member: refusedMember(outcome.detail) }
          : null,
      );
    },
  }));
  const formValues = useStore(form.store, (state) => state.values);
  const [locationListOptions, setLocationListOptions] = createSignal<LocationListRow[]>([]);
  const readLocationListOptions = async () => {
    const outcome = await locationList(props.transport, [
      { requestId: newRequestId() } as LocationListRequest,
    ]);
    if (outcome.status === "completed") {
      setLocationListOptions(outcome.value.rows as LocationListRow[]);
    }
  };
  void readLocationListOptions();
  const [receivingLoadReceiptScreenOptions, setReceivingLoadReceiptScreenOptions] = createSignal<ReceivingLoadReceiptScreenRow[]>([]);
  const readReceivingLoadReceiptScreenOptions = async (narrowed: string | null) => {
    if (narrowed === null || narrowed === "") {
      setReceivingLoadReceiptScreenOptions([]);
      return;
    }
    const outcome = await receivingLoadReceiptScreen(props.transport, [
      { requestId: newRequestId(), purchaseOrderId: narrowed } as ReceivingLoadReceiptScreenRequest,
    ]);
    if (outcome.status === "completed") {
      setReceivingLoadReceiptScreenOptions(outcome.value.rows as ReceivingLoadReceiptScreenRow[]);
    }
  };
  createEffect(() => {
    formValues();
    const narrowed = form.getFieldValue(`value.purchaseOrderId`) as string | null;
    void readReceivingLoadReceiptScreenOptions(narrowed ?? null);
  });
  const [purchaseOrderQueryOptions, setPurchaseOrderQueryOptions] = createSignal<PurchaseOrderQueryRow[]>([]);
  const readPurchaseOrderQueryOptions = async () => {
    const outcome = await purchaseOrderQuery(props.transport, [
      { requestId: newRequestId() } as PurchaseOrderQueryRequest,
    ]);
    if (outcome.status === "completed") {
      setPurchaseOrderQueryOptions(outcome.value.item as PurchaseOrderQueryRow[]);
    }
  };
  void readPurchaseOrderQueryOptions();

  return (
    <form
      onSubmit={(event) => {
        event.preventDefault();
        void form.handleSubmit();
      }}
    >
      <Show when={refusal()?.member === null ? refusal() : undefined}>
        <p>{refusal()?.code}</p>
      </Show>
      <form.Field name={"value.line"} mode="array">
        {(group) => (
          <fieldset>
            <legend>line</legend>
            <For each={group().state.value ?? []}>
              {(_, index) => (
                <fieldset>
                  <form.Field name={`value.line[${index()}].locationId`}>
                    {(field) => (
                      <label>
                        Location
                        <select
                          value={String(field().state.value ?? "")}
                          onChange={(event) => field().handleChange(event.currentTarget.value)}
                        >
                          <option value=""></option>
                          <For each={locationListOptions()}>
                            {(row) => (
                              <option
                                value={String(row.id)}
                                selected={String(field().state.value ?? "") === String(row.id)}
                              >
                                {String(row.locationCode)}
                              </option>
                            )}
                          </For>
                        </select>
                        <Show when={refusalMarks(refusal()?.member ?? null, "value.line[].location_id", index())}>
                          <em>{refusal()?.code}</em>
                        </Show>
                      </label>
                    )}
                  </form.Field>
                  <form.Field name={`value.line[${index()}].purchaseOrderLineId`}>
                    {(field) => (
                      <label>
                        Order line
                        <select
                          value={String(field().state.value ?? "")}
                          onChange={(event) => field().handleChange(event.currentTarget.value)}
                        >
                          <option value=""></option>
                          <For each={receivingLoadReceiptScreenOptions()}>
                            {(row) => (
                              <option
                                value={String(row.lineId)}
                                selected={String(field().state.value ?? "") === String(row.lineId)}
                              >
                                {String(row.itemNumber)}
                              </option>
                            )}
                          </For>
                        </select>
                        <Show when={refusalMarks(refusal()?.member ?? null, "value.line[].purchase_order_line_id", index())}>
                          <em>{refusal()?.code}</em>
                        </Show>
                      </label>
                    )}
                  </form.Field>
                  <form.Field name={`value.line[${index()}].quantity`}>
                    {(field) => (
                      <label>
                        Quantity
                        <input
                          type="text"
                          value={String(field().state.value ?? "")}
                          onInput={(event) => field().handleChange(event.currentTarget.value)}
                        />
                        <Show when={refusalMarks(refusal()?.member ?? null, "value.line[].quantity", index())}>
                          <em>{refusal()?.code}</em>
                        </Show>
                      </label>
                    )}
                  </form.Field>
                  <button
                    type="button"
                    disabled={!canRemove(group().state.value ?? [], 1)}
                    onClick={() => group().removeValue(index())}
                  >
                    remove
                  </button>
                </fieldset>
              )}
            </For>
            <button
              type="button"
              disabled={!canAdd(group().state.value ?? [], 100)}
              onClick={() => group().pushValue({} as NonNullable<NonNullable<NonNullable<ReceivingRecordReceiptRequest>["value"]>["line"]>[number])}
            >
              add
            </button>
          </fieldset>
        )}
      </form.Field>
      <form.Field name={`value.purchaseOrderId`}>
        {(field) => (
          <label>
            Purchase order
            <select
              value={String(field().state.value ?? "")}
              onChange={(event) => field().handleChange(event.currentTarget.value)}
            >
              <option value=""></option>
              <For each={purchaseOrderQueryOptions()}>
                {(row) => (
                  <option
                    value={String(row.id)}
                    selected={String(field().state.value ?? "") === String(row.id)}
                  >
                    {String(row.purchaseOrderNumber)}
                  </option>
                )}
              </For>
            </select>
            <Show when={refusalMarks(refusal()?.member ?? null, "value.purchase_order_id")}>
              <em>{refusal()?.code}</em>
            </Show>
          </label>
        )}
      </form.Field>
      <form.Field name={`value.receiptReference`}>
        {(field) => (
          <label>
            Receipt reference
            <input
              type="text"
              value={String(field().state.value ?? "")}
              onInput={(event) => field().handleChange(event.currentTarget.value)}
            />
            <Show when={refusalMarks(refusal()?.member ?? null, "value.receipt_reference")}>
              <em>{refusal()?.code}</em>
            </Show>
          </label>
        )}
      </form.Field>
      <button type="submit">submit</button>
    </form>
  );
}
