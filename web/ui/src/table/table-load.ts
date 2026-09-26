/**
 * The load of one DataTable over a table definition.
 *
 * A query streams its load: the caller opens the stream with the cap and the
 * sort this load asks for, and hands its rows over in batches. A bounded list
 * is one read, with the limit and the sort. A cap change, a sort on a set that
 * is not fully read and a refresh each start a new load, through the load
 * state of `@wamn/web-runtime`. A new load aborts the stream of the last one,
 * and a batch or an outcome of an older load is dropped.
 *
 * While an inline edit is open, the load is held: a new load waits until the
 * edit is saved or dropped, and then the last one asked for runs.
 */

import { createSignal, type Accessor } from "solid-js";

import {
  appendRows,
  emptyLoad,
  endLoad,
  finishLoad,
  loadLimit,
  replaceRow as replaceLoadedRow,
  type RowKey,
  startLoad,
  type LoadEnd,
  type LoadPage,
  type LoadState,
  type Outcome,
} from "@wamn/web-runtime";

import type { DataTableSort, DataTableSortDirection } from "./data-table";

/** The row cap of a table before the operator changes it. */
export const DEFAULT_CAP = 1000;

/** The parts of a table definition that a load reads. */
export interface TableLoadDefinition<TRow extends object> {
  /** The fields whose values together name each row. */
  readonly rowId: RowKey<TRow>;
  /** The most rows one read returns, or null when the read declares no limit. */
  readonly pageMaximum: number | null;
  /** The fields the read can sort by, each with the name its request sends. */
  readonly sortFields: readonly { readonly field: keyof TRow & string; readonly wire: string }[];
}

/** The sort one read sends: the wire name of one field and its direction. */
export interface TableLoadSort {
  readonly field: string;
  readonly direction: DataTableSortDirection;
}

/**
 * Opens the streamed load of `cap` rows in `sort`, hands its rows to `onRows`
 * in batches, and returns how it ended. `signal` aborts it.
 */
export type TableStream<TRow extends object> = (
  cap: number,
  sort: TableLoadSort | undefined,
  signal: AbortSignal,
  onRows: (rows: readonly TRow[]) => void,
) => Promise<LoadEnd>;

export interface TableLoad<TRow extends object> {
  /** The load state that the DataTable renders. */
  readonly state: Accessor<LoadState<TRow>>;
  /** Starts a new load, at the cap in force or at a new one. */
  readonly load: (cap?: number) => Promise<void>;
  /** Takes the sort of a header click and starts a new load in that order. */
  readonly sortBy: (sort: readonly DataTableSort<TRow>[]) => void;
  /** Holds every new load while `held` is true, and runs the last one asked for when it ends. */
  readonly hold: (held: boolean) => void;
  /** Puts one row, as a write left it, in place of the loaded row with its id. */
  readonly replaceRow: (row: TRow) => void;
}

export function createTableLoad<TRow extends object>(
  definition: TableLoadDefinition<TRow>,
  read: (limit: number, sort: TableLoadSort | undefined) => Promise<Outcome<LoadPage<TRow>>>,
  stream?: TableStream<TRow>,
): TableLoad<TRow> {
  const [state, setState] = createSignal<LoadState<TRow>>(emptyLoad(DEFAULT_CAP));
  let sort: TableLoadSort | undefined;
  let held = false;
  // The cap of the last load asked for while held, or undefined.
  let waiting: number | undefined;
  // The stream of the load in flight, which a new load aborts.
  let streaming: AbortController | undefined;

  const load = async (cap: number = state().cap) => {
    if (held) {
      waiting = cap;
      return;
    }
    const next = startLoad(state(), cap);
    setState(next);
    if (stream !== undefined) {
      streaming?.abort();
      const controller = new AbortController();
      streaming = controller;
      const end = await stream(next.cap, sort, controller.signal, (rows) => {
        // The table renders each batch at once, so the update's time is its
        // render time: the User Timing entry `wamn:load-render` (wamn-utci.6).
        const start = performance.now();
        setState((current) => appendRows(current, next.generation, rows));
        performance.measure("wamn:load-render", { start, detail: { rows: rows.length } });
      });
      setState((current) => endLoad(current, next.generation, end, definition.rowId));
      return;
    }
    const limit =
      definition.pageMaximum === null ? next.cap : loadLimit(next.cap, definition.pageMaximum);
    const outcome = await read(limit, sort);
    setState((current) => finishLoad(current, next.generation, outcome, definition.rowId));
  };

  // A read sorts by one field, so the load sends the first field of the sort.
  const sortBy = (sorts: readonly DataTableSort<TRow>[]) => {
    const [first] = sorts;
    const declared = definition.sortFields.find((field) => field.field === first?.field);
    if (first !== undefined && declared !== undefined) {
      sort = { field: declared.wire, direction: first.direction };
      void load();
    }
  };

  const hold = (next: boolean) => {
    held = next;
    const cap = waiting;
    if (!held && cap !== undefined) {
      waiting = undefined;
      void load(cap);
    }
  };

  const replaceRow = (row: TRow) => {
    setState((current) => replaceLoadedRow(current, row, definition.rowId));
  };

  return { state, load, sortBy, hold, replaceRow };
}
