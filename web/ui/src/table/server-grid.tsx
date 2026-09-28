/**
 * The rows of a set that is not fully read, as the read returned them
 * (wamn-5pzt).
 *
 * A QueryTable renders it until a load reads every row. It filters, groups
 * and sorts nothing itself. A header click on a declared sort field asks the
 * read for that order, and only those fields sort. The column arrangement
 * works as it does over a complete set.
 */

import { type ColumnDef, createTable, functionalUpdate, type SortingState } from "@tanstack/solid-table";
import { createMemo, type JSX, mergeProps } from "solid-js";

import { ColumnHeader } from "./column-menu";
import { type BuiltColumn, leadingIds, type TableSort } from "./columns";
import { Grid, type GridDetail, type GridFeatures, gridFeatures, gridViewOptions } from "./grid";
import type { GridViewState } from "./grid-view";

export function ServerGrid<TRow extends object>(props: {
  rows: readonly TRow[];
  rowId: (row: TRow) => string;
  columns: readonly BuiltColumn<TRow>[];
  /** The fields the read can sort by. */
  sortFields: readonly string[];
  sortMaxFields: number;
  grid: GridViewState;
  onGrid: (next: GridViewState) => void;
  sort: readonly TableSort[];
  onSort: (next: readonly TableSort[]) => void;
  busy: boolean;
  emptyMessage: JSX.Element;
  detail: GridDetail<TRow> | undefined;
  class: string;
}): JSX.Element {
  const grid = gridViewOptions(
    () => props.grid,
    (next) => props.onGrid(next),
    () => leadingIds(props.columns as readonly BuiltColumn<object>[]),
  );
  // Only a declared sort field sorts, so the header shows only those.
  const sorting = createMemo<SortingState>(() =>
    props.sort
      .filter((sort) => props.sortFields.includes(sort.field))
      .map((sort) => ({ id: sort.field, desc: sort.direction === "descending" })),
  );
  const columns = props.columns.map((built): ColumnDef<GridFeatures, TRow> =>
    built.declared === undefined
      ? {
          id: built.id,
          header: built.header ?? "",
          size: built.size,
          cell: (context) => built.cell(context.row.original, context.row.id),
          enableSorting: false,
          enableHiding: false,
          enablePinning: false,
          enableResizing: false,
        }
      : {
          id: built.id,
          header: (context) => <ColumnHeader column={context.column} label={built.declared!.label} />,
          size: built.size,
          cell: (context) => built.cell(context.row.original, context.row.id),
          accessorFn: (row) => row[built.declared!.field],
          enableSorting: props.sortFields.includes(built.id),
        },
  );
  const table = createTable({
    features: gridFeatures,
    get data() {
      return props.rows as TRow[];
    },
    columns,
    getRowId: (row) => props.rowId(row),
    state: mergeProps(grid.state, {
      get sorting() {
        return sorting();
      },
    }),
    onColumnOrderChange: grid.onColumnOrderChange,
    onColumnVisibilityChange: grid.onColumnVisibilityChange,
    onColumnSizingChange: grid.onColumnSizingChange,
    onColumnPinningChange: grid.onColumnPinningChange,
    onSortingChange: (updater) =>
      props.onSort(
        functionalUpdate(updater, sorting()).map((sort) => ({
          field: sort.id,
          direction: sort.desc ? "descending" : "ascending",
        })),
      ),
    manualSorting: true,
    get enableMultiSort() {
      return props.sortMaxFields > 1;
    },
    get maxMultiSortColCount() {
      return props.sortMaxFields;
    },
    columnResizeMode: "onChange",
    // A click turns ascending, then descending, and never clears the sort.
    sortDescFirst: false,
    enableSortingRemoval: false,
  });
  return (
    <Grid table={table} busy={props.busy} emptyMessage={props.emptyMessage} class={props.class} detail={props.detail} />
  );
}
