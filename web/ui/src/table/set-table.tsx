/**
 * The set table: every row of a complete set, and what the table does with it
 * (wamn-vfvx, wamn-5pzt).
 *
 * It takes the rows, the columns as the grid renders them, and its state, and
 * nothing about where the rows came from. It never loads, and every control it
 * shows applies, because a caller places it only over a complete set.
 *
 * - A header click sorts, in the table. A shift click adds a field, up to the
 *   sort's size.
 * - Each column header has a refine filter, and the bar shows a chip for each
 *   active filter. One search box keeps the rows whose shown text contains the
 *   search, in any case, in a visible column that is not json or bytes.
 * - The group bar nests the rows by one column for each level. A group row
 *   shows its value, its row count, and each other column's aggregate, and
 *   expands on its own. The groups of a level sort by their value or by one
 *   column's aggregate, and the rows inside keep the table sort. A grouped
 *   time groups by its day, week or month.
 * - The footer shows each column's aggregate over every row the filters and
 *   the search keep.
 * - The export button saves those rows as CSV, in the order the table shows
 *   them, with the visible columns in their order and each cell's shown text.
 *   Group rows and the footer are not exported.
 *
 * TanStack runs the rows through the filters, the groups and the sort, in that
 * order. Each groupable field has a hidden grouping column for each bucket, so
 * a grouping value never outlives its bucket. The sort starts with one entry
 * for each group level, whose column orders the groups of that level, and
 * then the table sort, which orders the rows inside.
 */

import {
  type ColumnDef,
  columnFilteringFeature,
  columnGroupingFeature,
  createExpandedRowModel,
  createFilteredRowModel,
  createGroupedRowModel,
  createSortedRowModel,
  createTable,
  functionalUpdate,
  globalFilteringFeature,
  type Row,
  rowAggregationFeature,
  rowExpandingFeature,
  type SortingState,
  tableFeatures,
} from "@tanstack/solid-table";
import ChevronDown from "lucide-solid/icons/chevron-down";
import ChevronRight from "lucide-solid/icons/chevron-right";
import X from "lucide-solid/icons/x";
import { createMemo, createSignal, createUniqueId, For, type JSX, mergeProps, onMount, Show } from "solid-js";

import { Badge } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { Field, FieldLabel } from "../components/ui/field";
import { Input } from "../components/ui/input";
import {
  type Aggregate,
  aggregateValues,
  allowedAggregates,
  type Bucket,
  BUCKETS,
  bucketLabel,
  bucketOf,
  compareAggregates,
  defaultAggregate,
} from "./aggregate";
import { ColumnFilter, filterMatches, filterText, type SetFilter } from "./column-filter";
import { ColumnHeader } from "./column-menu";
import {
  type BuiltColumn,
  compareValues,
  decimalScale,
  isEmpty,
  leadingIds,
  shownText,
  type TableColumn,
  type TableColumnType,
  type TableSort,
} from "./columns";
import { csvFileName, csvText, downloadCsv } from "./csv";
import { Grid, type GridDetail, gridFeatures, GridFootCell, GridFootRow, gridViewOptions, sameMemo } from "./grid";
import { same } from "./view-parts";
import type { GridViewState } from "./grid-view";
import { GroupBar, VALUE_SORT } from "./group-bar";
import type { GroupLevel, SetViewState } from "./set-view";

/** The features of a set table: the grid's, and the filters, the groups and the aggregates. */
export const setFeatures = tableFeatures({
  ...gridFeatures,
  columnFilteringFeature,
  // globalFilteringFeature requires columnFilteringFeature, declared above.
  globalFilteringFeature,
  columnGroupingFeature,
  rowAggregationFeature,
  rowExpandingFeature,
  filteredRowModel: createFilteredRowModel(),
  groupedRowModel: createGroupedRowModel(),
  sortedRowModel: createSortedRowModel(),
  expandedRowModel: createExpandedRowModel(),
});

export type SetFeatures = typeof setFeatures;

export interface SetTableProps<TRow extends object> {
  /** The table's name, which the file name of a CSV export starts with. */
  readonly name: string;
  readonly rows: readonly TRow[];
  readonly rowId: (row: TRow) => string;
  /** The columns as the grid renders them, the control columns among them. */
  readonly columns: readonly BuiltColumn<TRow>[];
  readonly view: SetViewState;
  readonly onView: (next: SetViewState) => void;
  readonly grid: GridViewState;
  readonly onGrid: (next: GridViewState) => void;
  readonly sort: readonly TableSort[];
  readonly onSort: (next: readonly TableSort[]) => void;
  /** How many fields one sort can hold. Above 1, a shift click adds a field. */
  readonly sortMaxFields: number;
  readonly detail?: GridDetail<TRow> | undefined;
  /** What the grid says when the set has no rows. */
  readonly emptyMessage?: JSX.Element | undefined;
  /** Classes of the grid's box, which the caller sizes. */
  readonly gridClass?: string | undefined;
  /** The time zone of a time's bucket. The default is the browser's zone. */
  readonly timeZone?: string | undefined;
  /** The ISO weekday a week bucket starts on, 1 for Monday, the default. */
  readonly weekStart?: number | undefined;
}

/** The label of the group of empty values. */
const NONE = "(none)";

/** True when a cell's shown text contains the search, in any case. */
const searchMatches = (value: unknown, search: string) =>
  value !== null && value !== undefined && String(value).toLowerCase().includes(search.toLowerCase());

const groupable = (type: TableColumnType) => type !== "json" && type !== "bytes";

/** The id of the hidden column that groups a field, by a bucket for a time. */
const groupingId = (field: string, bucket: Bucket | null) => `${field}@${bucket ?? "value"}`;

/** What a control column is to every TanStack feature: none of them apply. */
const CONTROL = {
  enableSorting: false,
  enableColumnFilter: false,
  enableGlobalFilter: false,
  enableGrouping: false,
  enableHiding: false,
  enablePinning: false,
  enableResizing: false,
} as const;

type SetRow<TRow extends object> = Row<SetFeatures, TRow>;

export function SetTable<TRow extends object>(props: SetTableProps<TRow>): JSX.Element {
  const timeZone = () => props.timeZone ?? Intl.DateTimeFormat().resolvedOptions().timeZone;
  const weekStart = () => props.weekStart ?? 1;
  const declared = createMemo(() =>
    props.columns.flatMap((built) => (built.declared === undefined ? [] : [built.declared])),
  );
  const column = (field: string) => declared().find((candidate) => candidate.field === field)!;
  const aggregateOf = (field: string): Aggregate =>
    props.view.aggregates[field] ?? defaultAggregate(column(field).type, column(field).role ?? "value");
  const levels = () => props.view.group;
  const grouped = (field: string) => levels().some((level) => level.field === field);
  // A choice field reports its value again when its options change, so a change
  // that changes nothing writes nothing.
  const setView = (change: Partial<SetViewState>) => {
    const next = { ...props.view, ...change };
    if (!same(next, props.view)) {
      props.onView(next);
    }
  };

  /** The scale of each numeric column: the most decimal places of any value. */
  const scales = createMemo(() =>
    Object.fromEntries(
      declared()
        .filter((candidate) => candidate.type === "numeric")
        .map((candidate) => [
          candidate.field,
          props.rows.reduce(
            (scale, row) =>
              isEmpty(row[candidate.field]) ? scale : Math.max(scale, decimalScale(String(row[candidate.field]))),
            0,
          ),
        ]),
    ),
  );

  const aggregateRows = (field: string, rows: readonly SetRow<TRow>[]) =>
    aggregateValues(
      column(field).type,
      aggregateOf(field),
      rows.map((row) => row.original[field as keyof TRow]),
      scales()[field] ?? 0,
    );

  /** The data rows under a group row, without the group rows nested in it. */
  const dataRows = (row: SetRow<TRow>) => row.getLeafRows().filter((leaf) => !leaf.getIsGrouped());

  /**
   * A group row's value, as its first data row gives it. TanStack keys a group
   * by the text of its value, so its own `groupingValue` holds "null" for the
   * group of empty values.
   */
  const groupValue = (row: SetRow<TRow>) => dataRows(row)[0]?.getGroupingValue(row.groupingColumnId!);

  /** The order of the groups of one level, which its hidden grouping column sorts by. */
  const groupOrder = (field: string, a: SetRow<TRow>, b: SetRow<TRow>): number => {
    const level: GroupLevel | undefined = levels()[a.depth];
    if (!a.getIsGrouped() || level?.field !== field) {
      return 0;
    }
    const { by } = level.sort;
    return by === "value"
      ? compareValues(level.bucket === null ? column(field).type : "text", groupValue(a), groupValue(b))
      : compareAggregates(
          column(by).type,
          aggregateOf(by),
          a.getValue(by) as ReturnType<typeof aggregateValues>,
          b.getValue(by) as ReturnType<typeof aggregateValues>,
        );
  };

  /** One declared column's cell: a group's value and count, a group's aggregate, or the row's own cell. */
  const cell = (built: BuiltColumn<TRow>, field: string, row: SetRow<TRow>): JSX.Element => {
    if (!row.getIsGrouped()) {
      return grouped(field) ? "" : built.cell(row.original, row.id);
    }
    if (row.groupingColumnId?.split("@")[0] === field) {
      const value = groupValue(row);
      const level = levels()[row.depth];
      return (
        <Button
          type="button"
          variant="ghost"
          size="sm"
          class="-ms-2"
          aria-expanded={row.getIsExpanded()}
          onClick={() => row.toggleExpanded()}
        >
          <Show when={row.getIsExpanded()} fallback={<ChevronRight aria-hidden="true" />}>
            <ChevronDown aria-hidden="true" />
          </Show>
          <span data-slot="table-group-value">
            {isEmpty(value) ? NONE : level?.bucket ? bucketLabel(String(value), level.bucket) : String(value)}
          </span>
          <span class="text-muted-foreground">({dataRows(row).length})</span>
        </Button>
      );
    }
    return grouped(field) ? "" : shownText(row.getValue(field));
  };

  const dataColumn = (built: BuiltColumn<TRow>, declaredColumn: TableColumn<TRow>): ColumnDef<SetFeatures, TRow> => {
    const { field, type, label } = declaredColumn;
    return {
      id: field,
      header: (context) => (
        <ColumnHeader
          column={context.column}
          label={label}
          filter={<ColumnFilter column={context.column} type={type} label={label} />}
          aggregate={{
            allowed: allowedAggregates(type),
            chosen: aggregateOf(field),
            onChoose: (aggregate) => setView({ aggregates: { ...props.view.aggregates, [field]: aggregate } }),
          }}
        />
      ),
      cell: (context) => cell(built, field, context.row),
      accessorFn: (row) => row[field],
      size: built.size,
      sortFn: (a, b) => (a.getIsGrouped() ? 0 : compareValues(type, a.original[field], b.original[field])),
      filterFn: Object.assign(
        (row: SetRow<TRow>, _id: string, filter: SetFilter) => filterMatches(type, row.original[field], filter),
        { autoRemove: (filter: unknown) => filter === undefined },
      ),
      enableGlobalFilter: groupable(type),
      enableGrouping: false,
      aggregationFn: {
        aggregate: (context) =>
          aggregateRows(field, context.groupingRow === undefined ? context.rows : dataRows(context.groupingRow)),
      },
    };
  };

  /** The hidden column that groups a field by one bucket, and orders its groups. */
  const groupingColumn = (declaredColumn: TableColumn<TRow>, bucket: Bucket | null): ColumnDef<SetFeatures, TRow> => {
    const { field } = declaredColumn;
    return {
      id: groupingId(field, bucket),
      header: "",
      accessorFn: (row) => row[field],
      // Every empty value joins one group, and a time groups by its bucket.
      getGroupingValue: (row) =>
        bucket === null
          ? isEmpty(row[field])
            ? null
            : row[field]
          : bucketOf(row[field], bucket, timeZone(), weekStart()),
      sortFn: (a, b) => groupOrder(field, a, b),
      enableColumnFilter: false,
      enableGlobalFilter: false,
      enablePinning: false,
    };
  };

  // The column definitions change only with the columns. TanStack copies each
  // definition once, so a definition reads the state when it runs.
  const columns = createMemo(() => [
    ...props.columns.map((built) =>
      built.declared === undefined
        ? ({
            ...CONTROL,
            id: built.id,
            header: built.header ?? "",
            size: built.size,
            cell: (context) => (context.row.getIsGrouped() ? null : built.cell(context.row.original, context.row.id)),
          } satisfies ColumnDef<SetFeatures, TRow>)
        : dataColumn(built, built.declared),
    ),
    ...declared()
      .filter((candidate) => groupable(candidate.type))
      .flatMap((candidate) =>
        candidate.type === "timestamptz"
          ? BUCKETS.map((bucket) => groupingColumn(candidate, bucket))
          : [groupingColumn(candidate, null)],
      ),
  ]);
  const groupingColumns = createMemo(() =>
    columns()
      .map((definition) => definition.id!)
      .filter((id) => id.includes("@")),
  );

  // The grouping names the chosen aggregates too, so a new choice rebuilds the
  // groups, whose aggregates it changes.
  const grouping = sameMemo(() => ({
    ids: levels().map((level) => groupingId(level.field, level.bucket)),
    aggregates: props.view.aggregates,
  }));
  // The sort names what each level orders its groups by, and the aggregates,
  // so a new choice of either sorts the groups again.
  const sorting = sameMemo(() => ({
    entries: [
      ...levels().map((level) => ({ id: groupingId(level.field, level.bucket), desc: level.sort.descending })),
      ...props.sort.map((sort) => ({ id: sort.field, desc: sort.direction === "descending" })),
    ] as SortingState,
    by: levels().map((level) => level.sort.by),
    aggregates: props.view.aggregates,
  }));
  const searchable = createMemo(
    () =>
      new Set<string>(
        declared()
          .filter((candidate) => groupable(candidate.type) && !props.grid.hidden.includes(candidate.field))
          .map((candidate) => candidate.field),
      ),
  );
  // The filter value names the searched columns too, so a change of the
  // visible columns filters again while a search is active.
  const globalFilter = createMemo(() =>
    props.view.search === "" ? undefined : { text: props.view.search, columns: [...searchable()] },
  );
  const columnFilters = sameMemo(() =>
    props.view.filters.map((active) => ({ id: active.field, value: active.filter })),
  );

  const grid = gridViewOptions(
    () => props.grid,
    (next) => props.onGrid(next),
    // The control columns lead, then the grouped fields, then the rest.
    () => [...leadingIds(props.columns as readonly BuiltColumn<object>[]), ...levels().map((level) => level.field)],
    groupingColumns,
  );

  const table = createTable({
    features: setFeatures,
    get data() {
      return props.rows as TRow[];
    },
    get columns() {
      return columns();
    },
    getRowId: (row) => props.rowId(row),
    state: mergeProps(grid.state, {
      get sorting() {
        return sorting().entries;
      },
      get grouping() {
        return grouping().ids;
      },
      get columnFilters() {
        return columnFilters();
      },
      get globalFilter() {
        return globalFilter();
      },
    }),
    onColumnOrderChange: grid.onColumnOrderChange,
    onColumnVisibilityChange: grid.onColumnVisibilityChange,
    onColumnSizingChange: grid.onColumnSizingChange,
    onColumnPinningChange: grid.onColumnPinningChange,
    onSortingChange: (updater) => {
      const next = functionalUpdate(updater, sorting().entries);
      props.onSort(
        next
          .filter((sort) => !sort.id.includes("@"))
          .map((sort) => ({ field: sort.id, direction: sort.desc ? "descending" : "ascending" })),
      );
    },
    onColumnFiltersChange: (updater) =>
      setView({
        filters: functionalUpdate(updater, columnFilters()).map((active) => ({
          field: active.id,
          filter: active.value as SetFilter,
        })),
      }),
    groupedColumnMode: false,
    // A group stays expanded across a new set and a new grouping.
    autoResetExpanded: false,
    getRowCanExpand: (row) => row.subRows.length > 0,
    getColumnCanGlobalFilter: (candidate) => searchable().has(candidate.id),
    globalFilterFn: (row: SetRow<TRow>, columnId: string, search: { text: string }) =>
      searchMatches(row.getValue(columnId), search.text),
    get enableMultiSort() {
      return props.sortMaxFields > 1;
    },
    get maxMultiSortColCount() {
      return props.sortMaxFields + levels().length;
    },
    columnResizeMode: "onChange",
    // A click turns ascending, then descending, and never clears the sort.
    sortDescFirst: false,
    enableSortingRemoval: false,
  });

  // The declared columns the table shows, in its order: the ones a total and an export read.
  const shownColumns = () =>
    table
      .getVisibleLeafColumns()
      .flatMap((leaf) => (declared().some((candidate) => candidate.field === leaf.id) ? [column(leaf.id)] : []));

  /**
   * The footer: each shown column's aggregate over every kept row, in the
   * order of the grid's cells. A control column has an empty cell, so each
   * total sits under its column.
   *
   * The totals read every kept row for each column, so they wait for the
   * first frame: the rows paint first, and each total cell shows its
   * aggregate's name until its value is in (wamn-207q).
   */
  const [painted, setPainted] = createSignal(false);
  onMount(() => requestAnimationFrame(() => setTimeout(() => setPainted(true))));
  const totals = createMemo(() => {
    const rows = painted() ? table.getFilteredRowModel().rows : null;
    return table
      .getVisibleLeafColumns()
      .map((leaf) =>
        declared().some((candidate) => candidate.field === leaf.id)
          ? {
              field: leaf.id,
              aggregate: aggregateOf(leaf.id),
              value: rows === null ? null : aggregateRows(leaf.id, rows),
            }
          : { field: leaf.id, aggregate: null, value: null },
      );
  });

  /** Saves the kept rows as CSV, in the order the table shows them, without group rows or totals. */
  function exportCsv() {
    const shown = shownColumns();
    const rows = table.getSortedRowModel().flatRows.filter((row) => !row.getIsGrouped());
    const text = csvText([
      shown.map((candidate) => candidate.label),
      ...rows.map((row) => shown.map((candidate) => shownText(row.original[candidate.field]))),
    ]);
    downloadCsv(csvFileName(props.name, new Date()), text);
  }

  /** Expand or collapse every group of one level. */
  function expandLevel(depth: number, expanded: boolean) {
    const ids = table
      .getGroupedRowModel()
      .flatRows.filter((row) => row.getIsGrouped() && row.depth === depth)
      .map((row) => row.id);
    table.setExpanded((current) => {
      const next: Record<string, boolean> = current === true ? {} : { ...current };
      for (const id of ids) {
        if (expanded) {
          next[id] = true;
        } else {
          delete next[id];
        }
      }
      return next;
    });
  }

  const filters = () =>
    props.view.filters.flatMap((active) => {
      const filtered = declared().find((candidate) => candidate.field === active.field);
      return filtered === undefined
        ? []
        : [{ field: active.field, label: filtered.label, text: filterText(filtered.type, active.filter) }];
    });
  const removeFilter = (field: string) =>
    setView({ filters: props.view.filters.filter((active) => active.field !== field) });
  const setLevel = (field: string, change: Partial<GroupLevel>) =>
    setView({ group: levels().map((level) => (level.field === field ? { ...level, ...change } : level)) });
  const searchId = createUniqueId();

  return (
    <>
      <div data-slot="set-table-toolbar" class="flex shrink-0 flex-wrap items-end gap-2">
        <Field class="w-64">
          <FieldLabel for={searchId}>search</FieldLabel>
          <Input
            id={searchId}
            type="search"
            value={props.view.search}
            onInput={(event) => setView({ search: event.currentTarget.value })}
          />
        </Field>
        <Button type="button" variant="outline" onClick={exportCsv}>
          export CSV
        </Button>
      </div>
      <Show when={filters().length > 0}>
        <div data-slot="table-filters" class="flex w-full shrink-0 flex-wrap items-center gap-2">
          <For each={filters()}>
            {(filter) => (
              <Badge variant="outline" class="gap-1">
                {filter.label} {filter.text}
                <Button
                  type="button"
                  variant="ghost"
                  size="icon-xs"
                  aria-label={`remove filter ${filter.label}`}
                  onClick={() => removeFilter(filter.field)}
                >
                  <X aria-hidden="true" />
                </Button>
              </Badge>
            )}
          </For>
          <Button type="button" variant="ghost" size="sm" onClick={() => setView({ filters: [] })}>
            clear all
          </Button>
        </div>
      </Show>
      <GroupBar
        columns={declared().map((candidate) => ({
          field: candidate.field,
          label: candidate.label,
          time: candidate.type === "timestamptz",
          groupable: groupable(candidate.type),
          aggregate: aggregateOf(candidate.field),
        }))}
        grouping={levels().map((level) => level.field)}
        bucket={(field) => levels().find((level) => level.field === field)?.bucket ?? "day"}
        sort={(field) => levels().find((level) => level.field === field)!.sort}
        onGrouping={(next) =>
          setView({
            group: next.map(
              (field) =>
                levels().find((level) => level.field === field) ?? {
                  field,
                  bucket: column(field).type === "timestamptz" ? "day" : null,
                  sort: VALUE_SORT,
                },
            ),
          })
        }
        onBucket={(field, bucket) => setLevel(field, { bucket })}
        onSort={(field, sort) => setLevel(field, { sort })}
        onExpandLevel={expandLevel}
      />
      <Grid
        table={table}
        busy={false}
        emptyMessage={
          props.view.filters.length > 0 || props.view.search !== ""
            ? "No row matches the search and filters."
            : props.emptyMessage
        }
        class={props.gridClass}
        detail={props.detail}
        footer={
          <Show when={props.rows.length > 0}>
            <GridFootRow>
              <For each={totals()}>
                {(total) => (
                  <GridFootCell>
                    <Show when={total.aggregate !== null}>
                      <span data-slot="table-total" data-field={total.field}>
                        <span class="text-muted-foreground">{total.aggregate}</span> {shownText(total.value)}
                      </span>
                    </Show>
                  </GridFootCell>
                )}
              </For>
            </GridFootRow>
          </Show>
        }
      />
    </>
  );
}
