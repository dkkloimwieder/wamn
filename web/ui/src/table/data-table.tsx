/**
 * The platform table: the rows of one load, with its cap and its state.
 *
 * The table renders the load state it is given and never reads on its own.
 * The source of the load owns the rows, the times and the cap, and starts a
 * new load when the cap changes.
 *
 * The table declares the one static bundle, `gridFeatures`, and only the core
 * row model. It renders through `WindowedTable`, so a table above
 * `WINDOW_FROM` rows draws only the rows in view.
 */

import { createTable, type ColumnDef } from "@tanstack/solid-table";
import { type JSX, Show } from "solid-js";

import { DataGrid, DataGridContainer } from "../blocks/data-grid";
import { TextField } from "../fields";
import { gridFeatures, type GridFeatures } from "../grid";
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
}

export function DataTable<TRow extends object>(props: DataTableProps<TRow>): JSX.Element {
  const table = createTable({
    features: gridFeatures,
    get data() {
      return props.rows as TRow[];
    },
    get columns() {
      return props.columns.map(
        (column): ColumnDef<GridFeatures, TRow> => ({
          id: column.field,
          header: column.label,
          accessorFn: (row) => row[column.field],
        }),
      );
    },
    getRowId: (row) => String(row[props.rowId]),
    manualPagination: true,
  });

  return (
    <section data-slot="data-table" class="flex min-w-0 flex-col gap-4">
      <div data-slot="data-table-toolbar" class="flex flex-wrap items-end justify-between gap-4">
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
        <div role="status" class="flex flex-col items-end gap-1 text-sm text-muted-foreground">
          <Show when={props.startedAt}>
            {(at) => <p>started {at().toLocaleTimeString()}</p>}
          </Show>
          <Show when={props.endedAt}>{(at) => <p>ended {at().toLocaleTimeString()}</p>}</Show>
          <Show when={props.busy}>
            <p>Loading...</p>
          </Show>
          <Show when={!props.busy && !props.fullyRead}>
            <p class="text-foreground">Full dataset cannot be loaded</p>
          </Show>
        </div>
      </div>
      <DataGrid table={table} recordCount={props.rows.length} isLoading={props.busy}>
        <DataGridContainer>
          <WindowedTable />
        </DataGridContainer>
      </DataGrid>
    </section>
  );
}
