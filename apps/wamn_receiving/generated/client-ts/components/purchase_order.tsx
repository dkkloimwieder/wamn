// @generated from the client-contract IR; do not edit.
//
// `purchase_order` components. Each one calls the bindings and the runtime, and
// nothing else.

import { For, Show, createResource, createSignal } from "solid-js";
import {
  createSolidTable,
  flexRender,
  getCoreRowModel,
  type ColumnDef,
} from "@tanstack/solid-table";
import { createForm } from "@tanstack/solid-form";
import { z } from "zod";
import {
  appendPage,
  cellText,
  emptyPage,
  firstPage,
  hasNextPage,
  newRequestId,
  readMember,
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
  get,
  query,
  type PurchaseOrderGetRequest,
  type PurchaseOrderGetResult,
  type PurchaseOrderQueryRequest,
  type PurchaseOrderQueryResult,
  type PurchaseOrderQueryRow,
  type PurchaseOrderUpdateRequest,
  type PurchaseOrderUpdateResult,
  update,
} from "../purchase_order.js";

/** The record that the detail for `wamn-receiving:purchase-order/get@1.0.0` reads. */
export interface PurchaseOrderGetDetailInput {
  readonly id: Uuid;
}

/** What the detail screen for `wamn-receiving:purchase-order/get@1.0.0` takes. */
export interface PurchaseOrderGetDetailProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** The input that names the record. */
  readonly input: PurchaseOrderGetDetailInput;
  /** Called with every outcome this screen reads. */
  readonly onOutcome?: (outcome: Outcome<PurchaseOrderGetResult>) => void;
}

/**
 * The detail screen for `wamn-receiving:purchase-order/get@1.0.0`.
 *
 * It reads when it mounts and again whenever its input changes, because the
 * input names the record it shows.
 */
export function PurchaseOrderGetDetail(props: PurchaseOrderGetDetailProps) {
  const [outcome] = createResource(
    () => props.input,
    async (input: PurchaseOrderGetDetailInput) => {
      const read = await get(props.transport, [
        { ...input, requestId: newRequestId() } as PurchaseOrderGetRequest,
      ]);
      props.onOutcome?.(read);
      return read;
    },
  );
  const record = (): PurchaseOrderGetResult | undefined => {
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
        <dt>purchase order number</dt>
        <dd>{cellText(readMember(record(), ["purchaseOrderNumber"]), "text")}</dd>
        <dt>row version</dt>
        <dd>{cellText(readMember(record(), ["rowVersion"]), "int32")}</dd>
        <dt>status</dt>
        <dd>{cellText(readMember(record(), ["status"]), "text")}</dd>
        <dt>supplier id</dt>
        <dd>{cellText(readMember(record(), ["supplierId"]), "uuid")}</dd>
        <dt>updated at</dt>
        <dd>{cellText(readMember(record(), ["updatedAt"]), "timestamptz")}</dd>
        <dt>updated by</dt>
        <dd>{cellText(readMember(record(), ["updatedBy"]), "uuid")}</dd>
      </dl>
    </section>
  );
}

/** Columns of `wamn-receiving:purchase-order/query@1.0.0`, in contract order. */
const QUERY_COLUMNS: ColumnDef<PurchaseOrderQueryRow, unknown>[] = [
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
    accessorKey: "purchaseOrderNumber",
    header: "purchase order number",
    cell: (cell) => cellText(cell.getValue() as JsonValue, "text"),
  },
  {
    accessorKey: "rowVersion",
    header: "row version",
    cell: (cell) => cellText(cell.getValue() as JsonValue, "int32"),
  },
  {
    accessorKey: "status",
    header: "status",
    cell: (cell) => cellText(cell.getValue() as JsonValue, "text"),
  },
  {
    accessorKey: "supplierId",
    header: "supplier id",
    cell: (cell) => cellText(cell.getValue() as JsonValue, "uuid"),
  },
  {
    accessorKey: "updatedAt",
    header: "updated at",
    cell: (cell) => cellText(cell.getValue() as JsonValue, "timestamptz"),
  },
  {
    accessorKey: "updatedBy",
    header: "updated by",
    cell: (cell) => cellText(cell.getValue() as JsonValue, "uuid"),
  },
];

/** What the table for `wamn-receiving:purchase-order/query@1.0.0` takes. */
export interface PurchaseOrderQueryTableProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** Input the parent fixes, which the operator does not edit. */
  readonly fixed?: Partial<PurchaseOrderQueryRequest>;
  /** Called when the operator picks one row. */
  readonly onRowSelect?: (row: PurchaseOrderQueryRow) => void;
  /** Called when the operator opens `wamn-receiving:purchase-order/get@1.0.0` from one row. */
  readonly onOpenPurchaseOrderGet?: (row: PurchaseOrderQueryRow) => void;
  /** Called with every outcome this screen reads. */
  readonly onOutcome?: (outcome: Outcome<PurchaseOrderQueryResult>) => void;
}

/**
 * The table for `wamn-receiving:purchase-order/query@1.0.0`.
 *
 * It owns its page controls and its rows. A change to a control clears the
 * rows, because a cursor names a position in the list the old input produced.
 *
 * It reads when the operator asks, and not when it mounts, because a read is
 * a request that the operator did not send yet.
 */
export function PurchaseOrderQueryTable(props: PurchaseOrderQueryTableProps) {
  const [controls, setControls] = createSignal<Partial<PurchaseOrderQueryRequest>>({});
  const [page, setPage] = createSignal<PageState<PurchaseOrderQueryRow>>(emptyPage<PurchaseOrderQueryRow>());

  const read = async (cursor: string | null) => {
    setPage(startRead(page()));
    const request = {
      ...controls(),
      ...props.fixed,
      requestId: newRequestId(),
    } as PurchaseOrderQueryRequest;
    const sent = cursor === null ? request : (writeMember(request, ["cursor"], cursor) as PurchaseOrderQueryRequest);
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
    setPage(emptyPage<PurchaseOrderQueryRow>());
    void read(null);
  };

  const change = (path: readonly string[], value: JsonValue) => {
    setControls((current) => writeMember(current, path, value));
    restart();
  };

  const table = createSolidTable({
    get data() {
      return page().rows as PurchaseOrderQueryRow[];
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
          status
          <input type="text" onChange={(event) => change(["filter", "status"], event.currentTarget.value.split(",").filter((part) => part !== ""))} />
        </label>
        <label>
          supplier id
          <input type="text" onChange={(event) => change(["filter", "supplierId"], event.currentTarget.value.split(",").filter((part) => part !== ""))} />
        </label>
        <label>
          field
          <select onChange={(event) => change(["sort", "field"], event.currentTarget.value)}>
            <option value=""></option>
            <option value="created_at">created at</option>
            <option value="purchase_order_number">purchase order number</option>
            <option value="status">status</option>
          </select>
        </label>
        <label>
          direction
          <select onChange={(event) => change(["sort", "direction"], event.currentTarget.value)}>
            <option value=""></option>
            <option value="ascending">ascending</option>
            <option value="descending">descending</option>
          </select>
        </label>
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
                  <Show when={props.onOpenPurchaseOrderGet}>
                    <button type="button" onClick={() => props.onOpenPurchaseOrderGet?.(row.original)}>
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

/** What an operator types for `wamn-receiving:purchase-order/update@1.0.0`. */
const UPDATE_INPUT = z.object({
  change: z
    .object({
      supplierId: z.string().optional(),
    })
    .optional(),
});

/** What the form for `wamn-receiving:purchase-order/update@1.0.0` can start with. */
export interface PurchaseOrderUpdateFormInitial {
  change?: {
    supplierId?: Uuid;
  };
}

/** What the form for `wamn-receiving:purchase-order/update@1.0.0` takes. */
export interface PurchaseOrderUpdateFormProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** Values the form starts with. */
  readonly initial?: PurchaseOrderUpdateFormInitial;
  /** The record this command changes. The form reads it, and sends the
   * revision it read, because `wamn-receiving:purchase-order/get@1.0.0` states that binding. */
  readonly key: PurchaseOrderGetDetailInput;
  /** Called with the outcome of every submission. */
  readonly onSubmitted?: (outcome: Outcome<PurchaseOrderUpdateResult>) => void;
}

/**
 * The form for `wamn-receiving:purchase-order/update@1.0.0`.
 *
 * It renders what the operator fills and nothing else. The reserved inputs
 * come from the runtime at submit time, and the operator never sees them.
 */
export function PurchaseOrderUpdateForm(props: PurchaseOrderUpdateFormProps) {
  const [refusal, setRefusal] = createSignal<{ code: string | null; member: string | null } | null>(
    null,
  );

  const form = createForm(() => ({
    defaultValues: { ...props.initial } as Partial<PurchaseOrderUpdateRequest>,
    onSubmit: async ({ value }: { value: Partial<PurchaseOrderUpdateRequest> }) => {
      const checked = UPDATE_INPUT.safeParse(value);
      if (!checked.success) {
        const issue = checked.error.issues[0];
        setRefusal({
          code: issue?.message ?? "the input is not valid",
          member: typeof issue?.path.at(-1) === "string" ? String(issue.path.at(-1)) : null,
        });
        return;
      }
      let item = { ...value } as PurchaseOrderUpdateRequest;
      item = writeMember(item, ["requestId"], newRequestId());
      // The revision comes from the record this command changes, read
      // now, because a stale revision is what the conflict outcome names.
      const record = await get(props.transport, [
        { ...props.key, requestId: newRequestId() },
      ]);
      if (record.status !== "completed") {
        setRefusal({ code: record.status, member: null });
        return;
      }
      item = writeMember(item, ["id"], readMember(record.value, ["id"]) ?? null);
      item = writeMember(item, ["expectedRowVersion"], readMember(record.value, ["rowVersion"]) ?? null);
      const outcome = await update(props.transport, [item]);
      props.onSubmitted?.(outcome);
      setRefusal(
        outcome.status === "refused"
          ? { code: outcome.code, member: refusedMember(outcome.detail) }
          : null,
      );
    },
  }));

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
      <form.Field name={`change.supplierId`}>
        {(field) => (
          <label>
            supplier id
            <input
              type="text"
              value={String(field().state.value ?? "")}
              onInput={(event) => field().handleChange(event.currentTarget.value)}
            />
            <Show when={refusalMarks(refusal()?.member ?? null, "change.supplier_id")}>
              <em>{refusal()?.code}</em>
            </Show>
          </label>
        )}
      </form.Field>
      <button type="submit">submit</button>
    </form>
  );
}
