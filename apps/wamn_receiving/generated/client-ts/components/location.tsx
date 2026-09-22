// @generated from the client-contract IR; do not edit.
//
// `location` components. Each one calls the bindings and the runtime, and
// nothing else.

import { For, Show, createSignal } from "solid-js";
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
  startRead,
  type JsonValue,
  type Outcome,
  type PageState,
  type Transport,
} from "@wamn/web-runtime";
import {
  list,
  type LocationListRequest,
  type LocationListResult,
  type LocationListRow,
} from "../location.js";

/** Columns of `wamn-receiving:location/list@1.0.0`, in contract order. */
const LIST_COLUMNS: ColumnDef<LocationListRow, unknown>[] = [
  {
    accessorKey: "id",
    header: "id",
    cell: (cell) => cellText(cell.getValue() as JsonValue, "uuid"),
  },
  {
    accessorKey: "locationCode",
    header: "location code",
    cell: (cell) => cellText(cell.getValue() as JsonValue, "text"),
  },
];

/** What the table for `wamn-receiving:location/list@1.0.0` takes. */
export interface LocationListTableProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** Input the parent fixes, which the operator does not edit. */
  readonly fixed?: Partial<LocationListRequest>;
  /** Called when the operator picks one row. */
  readonly onRowSelect?: (row: LocationListRow) => void;
  /** Called with every outcome this screen reads. */
  readonly onOutcome?: (outcome: Outcome<LocationListResult>) => void;
}

/** What an operator calls this screen. The page decides where it goes. */
export const LocationListTableLabel = "list";

/**
 * The table for `wamn-receiving:location/list@1.0.0`.
 *
 * It owns its page controls and its rows. A change to a control clears the
 * rows, because a cursor names a position in the list the old input produced.
 *
 * It reads when the operator asks, and not when it mounts, because a read is
 * a request that the operator did not send yet.
 */
export function LocationListTable(props: LocationListTableProps) {
  const controls = (): Partial<LocationListRequest> => ({});
  const [page, setPage] = createSignal<PageState<LocationListRow>>(emptyPage<LocationListRow>());

  const read = async (cursor: string | null) => {
    setPage(startRead(page()));
    const request = {
      ...controls(),
      ...props.fixed,
      requestId: newRequestId(),
    } as LocationListRequest;
    const sent = request;
    const outcome = await list(props.transport, [sent]);
    props.onOutcome?.(outcome);
    if (outcome.status !== "completed") {
      setPage({ ...page(), busy: false });
      return;
    }
    const rows = outcome.value.rows;
    setPage(cursor === null ? firstPage(rows, null) : appendPage(page(), rows, null));
  };

  const restart = () => {
    setPage(emptyPage<LocationListRow>());
    void read(null);
  };

  const table = createSolidTable({
    get data() {
      return page().rows as LocationListRow[];
    },
    columns: LIST_COLUMNS,
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
