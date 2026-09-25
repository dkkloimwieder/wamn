/**
 * The platform table: the rows of one load, with its cap and its state.
 *
 * The table renders the load state it is given and never reads on its own.
 * The source of the load owns the rows, the times and the cap, and starts a
 * new load when the cap changes.
 *
 * The table declares one static bundle, `gridFeatures` with the sorted row
 * model. It renders through `WindowedTable`, so a table above `WINDOW_FROM`
 * rows draws only the rows in view.
 *
 * A header click sorts. The sort state lives in the table. If the set is fully
 * read, the table sorts its rows. If it is not, the table calls
 * `onSortChange` and does not sort, because the source must read again in the
 * new order. Then only the declared sort fields can sort.
 *
 * The table fills the height of its container, which the app sizes. The
 * toolbar stays above the grid, and the grid body is the only element that
 * scrolls, so it is the element the windowing measures against.
 */

import {
  createSortedRowModel,
  createTable,
  tableFeatures,
  type Column,
  type ColumnDef,
  type Row,
} from "@tanstack/solid-table";
import { ArrowDown, ArrowUp } from "lucide-solid";
import { createMemo, type JSX, Match, Show, Switch } from "solid-js";

import { DataGrid, DataGridContainer } from "../blocks/data-grid";
import { Button } from "../components/ui/button";
import { TextField } from "../fields";
import { gridFeatures } from "../grid";
import { WindowedTable } from "../windowed-table";

/** The type of a column, as the frozen `wamn:postgres/types.sql-value` names it. */
export type DataTableColumnType =
  | "boolean"
  | "int32"
  | "int64"
  | "float64"
  | "text"
  | "bytes"
  | "numeric"
  | "timestamptz"
  | "json"
  | "uuid";

/** A sort direction, as a table definition names it. */
export type DataTableSortDirection = "ascending" | "descending";

/** One field of a sort, in the order of the sort. */
export interface DataTableSort<TRow extends object> {
  readonly field: keyof TRow & string;
  readonly direction: DataTableSortDirection;
}

/** One column of the table. */
export interface DataTableColumn<TRow extends object> {
  readonly field: keyof TRow & string;
  readonly label: string;
  readonly type: DataTableColumnType;
}

export interface DataTableProps<TRow extends object> {
  readonly columns: readonly DataTableColumn<TRow>[];
  /** The field that holds the id of each row. */
  readonly rowId: keyof TRow & string;
  /** The rows loaded so far. */
  readonly rows: readonly TRow[];
  /** True when the load ended and no rows exist beyond the cap. */
  readonly fullyRead: boolean;
  /** True while a load is in progress. */
  readonly busy: boolean;
  /** The row cap in force. */
  readonly cap: number;
  /** Called with the new cap when the operator commits one. */
  readonly onCapChange: (cap: number) => void;
  /** When the load started, or null before the first load. */
  readonly startedAt: Date | null;
  /** When the load ended, or null while it runs. */
  readonly endedAt: Date | null;
  /** Why the last load did not complete, or null. */
  readonly refusal: string | null;
  /** Called when the operator asks for a new load of the same scope. */
  readonly onRefresh: () => void;
  /** The fields the source can sort by, which alone sort a set that is not fully read. */
  readonly sortFields: readonly { readonly field: keyof TRow & string }[];
  /** How many fields one sort can hold. Above 1, a shift click adds a field. */
  readonly sortMaxFields: number;
  /** Called with the whole new sort after a click, when the set is not fully read. */
  readonly onSortChange: (sort: readonly DataTableSort<TRow>[]) => void;
}

/** The feature bundle of every data table: the grid features and the sorted row model. */
const dataTableFeatures = tableFeatures({
  ...gridFeatures,
  sortedRowModel: createSortedRowModel(),
});

type DataTableFeatures = typeof dataTableFeatures;

/**
 * The order of two values of one column type. A null sorts after every value,
 * so a descending sort puts it first, as Postgres does.
 */
function compareValues(type: DataTableColumnType, a: unknown, b: unknown): number {
  if (a === null || a === undefined || b === null || b === undefined) {
    return (a === null || a === undefined ? 1 : 0) - (b === null || b === undefined ? 1 : 0);
  }
  switch (type) {
    case "int32":
    case "float64":
    case "numeric":
      return Number(a) - Number(b);
    case "int64": {
      const [x, y] = [BigInt(a as string), BigInt(b as string)];
      return x < y ? -1 : x > y ? 1 : 0;
    }
    case "boolean":
      return Number(a) - Number(b);
    default: {
      const [x, y] = [String(a), String(b)];
      return x < y ? -1 : x > y ? 1 : 0;
    }
  }
}

export function DataTable<TRow extends object>(props: DataTableProps<TRow>): JSX.Element {
  // The column definitions change only with the columns. A getter that built
  // them on every read would make the table rebuild its columns on every read.
  const columns = createMemo(() =>
    props.columns.map(
      (column): ColumnDef<DataTableFeatures, TRow> => ({
        id: column.field,
        header: (context) => (
          <SortHeader
            column={context.column}
            label={column.label}
            onSort={() => {
              if (!props.fullyRead) {
                props.onSortChange(
                  (table.atoms.sorting?.get() ?? []).map((sort) => ({
                    field: sort.id as keyof TRow & string,
                    direction: sort.desc ? "descending" : "ascending",
                  })),
                );
              }
            }}
          />
        ),
        accessorFn: (row) => row[column.field],
        sortFn: (a: Row<DataTableFeatures, TRow>, b: Row<DataTableFeatures, TRow>) =>
          compareValues(column.type, a.original[column.field], b.original[column.field]),
        // Read on each call, so a set that stops being fully read limits the sort.
        get enableSorting() {
          return props.fullyRead || props.sortFields.some((sort) => sort.field === column.field);
        },
      }),
    ),
  );
  const table = createTable({
    features: dataTableFeatures,
    get data() {
      return props.rows as TRow[];
    },
    get columns() {
      return columns();
    },
    getRowId: (row) => String(row[props.rowId]),
    manualPagination: true,
    get manualSorting() {
      return !props.fullyRead;
    },
    get enableMultiSort() {
      return props.sortMaxFields > 1;
    },
    get maxMultiSortColCount() {
      return props.sortMaxFields;
    },
    // A click turns ascending, then descending, and never clears the sort.
    sortDescFirst: false,
    enableSortingRemoval: false,
  });

  return (
    <section data-slot="data-table" class="flex h-full min-h-0 min-w-0 flex-col gap-4">
      <div
        data-slot="data-table-toolbar"
        class="flex shrink-0 flex-wrap items-end justify-between gap-4"
      >
        <div class="flex items-end gap-2">
          <div class="w-32">
            <TextField
              label="cap"
              type="number"
              min={1}
              value={String(props.cap)}
              onChange={(value) => {
                const cap = Number(value);
                if (Number.isInteger(cap) && cap > 0) {
                  props.onCapChange(cap);
                }
              }}
            />
          </div>
          <Button type="button" variant="outline" disabled={props.busy} onClick={() => props.onRefresh()}>
            refresh
          </Button>
        </div>
        <div role="status" class="flex flex-col items-end gap-1 text-sm text-muted-foreground">
          <Show when={props.startedAt}>
            {(at) => <p>started {at().toLocaleTimeString()}</p>}
          </Show>
          <Show when={props.endedAt}>{(at) => <p>ended {at().toLocaleTimeString()}</p>}</Show>
          <Show when={props.busy}>
            <p>Loading...</p>
          </Show>
          <Show when={!props.busy && !props.fullyRead && props.refusal === null}>
            <p class="text-foreground">Full dataset cannot be loaded</p>
          </Show>
        </div>
      </div>
      <DataGrid
        table={table}
        recordCount={props.rows.length}
        isLoading={props.busy}
        emptyMessage={props.refusal}
      >
        {/* The grid takes the height the toolbar leaves, in place of its fixed one. */}
        <DataGridContainer class="h-auto min-h-0 flex-1">
          <WindowedTable />
        </DataGridContainer>
      </DataGrid>
    </section>
  );
}

/** A header: its label, and a sort control when the column can sort. */
function SortHeader<TRow extends object>(props: {
  column: Column<DataTableFeatures, TRow, unknown>;
  label: string;
  onSort: () => void;
}): JSX.Element {
  return (
    <Show when={props.column.getCanSort()} fallback={props.label}>
      <Button
        type="button"
        variant="ghost"
        size="sm"
        class="-ms-2"
        onClick={(event: MouseEvent) => {
          props.column.getToggleSortingHandler()?.(event);
          props.onSort();
        }}
      >
        {props.label}
        <Switch>
          <Match when={props.column.getIsSorted() === "asc"}>
            <ArrowUp aria-hidden="true" />
          </Match>
          <Match when={props.column.getIsSorted() === "desc"}>
            <ArrowDown aria-hidden="true" />
          </Match>
        </Switch>
      </Button>
    </Show>
  );
}
