/**
 * The load of one DataTable over a table definition.
 *
 * The caller does the one page read, with the limit and the sort this load
 * asks for. A cap change, a sort on a set that is not fully read and a
 * refresh each start a new load, through the load state of
 * `@wamn/web-runtime`. An outcome of an older load is dropped.
 */

import { createSignal, type Accessor } from "solid-js";

import {
  emptyLoad,
  finishLoad,
  loadLimit,
  startLoad,
  type LoadPage,
  type LoadState,
  type Outcome,
} from "@wamn/web-runtime";

import type { DataTableSort, DataTableSortDirection } from "./data-table";

/** The row cap of a table before the operator changes it. */
export const DEFAULT_CAP = 1000;

/** The parts of a table definition that a load reads. */
export interface TableLoadDefinition<TRow extends object> {
  readonly rowId: keyof TRow & string;
  readonly pageMaximum: number;
  /** The fields the read can sort by, each with the name its request sends. */
  readonly sortFields: readonly { readonly field: keyof TRow & string; readonly wire: string }[];
}

/** The sort one read sends: the wire name of one field and its direction. */
export interface TableLoadSort {
  readonly field: string;
  readonly direction: DataTableSortDirection;
}

export interface TableLoad<TRow extends object> {
  /** The load state that the DataTable renders. */
  readonly state: Accessor<LoadState<TRow>>;
  /** Starts a new load, at the cap in force or at a new one. */
  readonly load: (cap?: number) => Promise<void>;
  /** Takes the sort of a header click and starts a new load in that order. */
  readonly sortBy: (sort: readonly DataTableSort<TRow>[]) => void;
}

export function createTableLoad<TRow extends object>(
  definition: TableLoadDefinition<TRow>,
  read: (limit: number, sort: TableLoadSort | undefined) => Promise<Outcome<LoadPage<TRow>>>,
): TableLoad<TRow> {
  const [state, setState] = createSignal<LoadState<TRow>>(emptyLoad(DEFAULT_CAP));
  let sort: TableLoadSort | undefined;

  const load = async (cap: number = state().cap) => {
    const next = startLoad(state(), cap);
    setState(next);
    const outcome = await read(loadLimit(next.cap, definition.pageMaximum), sort);
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

  return { state, load, sortBy };
}
