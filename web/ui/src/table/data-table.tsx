/**
 * The platform table: the rows of one load, with its cap and its state.
 *
 * The table renders the load state it is given and never reads on its own.
 * The source of the load owns the rows, the times and the cap, and starts a
 * new load when the cap changes.
 *
 * The table declares one static bundle: `gridFeatures`, column and global
 * filtering, grouping, aggregation, and the filtered, grouped, sorted and
 * expanded row models. The rows pass through them in that order. It renders
 * through `WindowedTable`, so a table above `WINDOW_FROM` rows draws only the
 * rows in view.
 *
 * A header click sorts. The sort state lives in the table. If the set is fully
 * read, the table sorts its rows. If it is not, the table calls
 * `onSortChange` and does not sort, because the source must read again in the
 * new order. Then only the declared sort fields can sort.
 *
 * Each column header has a refine filter, and the toolbar shows a chip for
 * each active filter. The filters run in the table, before the sort, and only
 * on a fully read set. On a set that is not fully read, the filters stay in
 * the table state, apply to no row, and their chips are disabled.
 *
 * One search box keeps the rows whose shown text contains the search, in any
 * case, in a visible column that is not json or bytes. It applies with the
 * filters, before the sort. On a set that is not fully read, it is disabled,
 * says why, and applies to no row.
 *
 * The group bar nests the rows by one column for each level. A group row
 * shows its value, its row count, and each other column's aggregate, and
 * expands on its own. The groups of a level sort by their value or by one
 * column's aggregate, and the rows inside keep the table sort. A group's id is
 * the ordered list of its fields and values, so a group stays expanded across
 * a new load. The footer shows each column's aggregate over every row the
 * filters and the search keep. Grouping and the footer apply only to a fully
 * read set, as the filters do.
 *
 * The export button saves the rows the filters and the search keep as CSV, in
 * the table sort, with the visible columns in their order and each cell's
 * shown text. Group rows and the footer are not exported. On a set that is
 * not fully read, the button is disabled and says why.
 *
 * A caller that passes `rowActions` gets one last column with the buttons of
 * each row, for example the row link that opens its record. That column does
 * not group, total or export, and it stays out of the column panel, the views
 * and the URL, which read the declared columns.
 *
 * Each header has a menu that sorts, hides, pins and unpins its column and
 * chooses its aggregate. The column panel shows and hides columns and orders
 * them, and a header edge drag sets a width. The arrangement works in every
 * mode. The scope bar holds the server part of a load: each declared scope
 * filter with its values, and the current sort as a chip. A scope change calls
 * `onScopeChange`, and the source reads again. The table applies no scope.
 *
 * A view is a named copy of all of this state and the cap, kept in memory for
 * the session. A table with a `urlKey` reads its state from the URL when it
 * draws and keeps the URL in step, in the canonical form of `view.ts`. On a
 * set that is not fully read, the scope, the cap and the sort of a view reach
 * the source, and the client parts wait, as the filters do.
 *
 * The table fills the height of its container, which the app sizes. The
 * toolbar stays above the grid, and the grid body is the only element that
 * scrolls, so it is the element the windowing measures against.
 */

import {
  columnFilteringFeature,
  columnGroupingFeature,
  createExpandedRowModel,
  createFilteredRowModel,
  createGroupedRowModel,
  createSortedRowModel,
  createTable,
  globalFilteringFeature,
  metaHelper,
  rowAggregationFeature,
  tableFeatures,
  type Cell,
  type Column,
  type ColumnDef,
  type Row,
  type RowModel,
  type Table,
} from "@tanstack/solid-table";
import { ArrowDown, ArrowUp, ChevronDown, ChevronRight, X } from "lucide-solid";
import {
  createEffect,
  createMemo,
  createSignal,
  createUniqueId,
  For,
  type JSX,
  Match,
  on,
  onMount,
  Show,
  Switch,
  untrack,
} from "solid-js";

import {
  DataGrid,
  DataGridContainer,
  DataGridTableFootRow,
  DataGridTableFootRowCell,
} from "../blocks/data-grid";
import { Badge } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { Field, FieldDescription, FieldLabel } from "../components/ui/field";
import { Input } from "../components/ui/input";
import { TextField } from "../fields";
import { gridFeatures } from "../grid";
import { WindowedTable } from "../windowed-table";
import {
  type AggregateResult,
  aggregateValues,
  allowedAggregates,
  bucketLabel,
  bucketOf,
  compareAggregates,
  type DataTableAggregate,
  type DataTableBucket,
  decimalScale,
  defaultAggregate,
  isEmpty,
} from "./aggregate";
import {
  ColumnFilter,
  type DataTableFilter,
  FILTER_NEEDS_FULL_SET,
  filterMatches,
  filterText,
} from "./column-filter";
import { csvFileName, csvText, downloadCsv, EXPORT_NEEDS_FULL_SET } from "./csv";
import { ColumnMenu } from "./column-menu";
import { ColumnPanel } from "./column-panel";
import { type DataTableGroupSort, GroupBar, VALUE_SORT } from "./group-bar";
import { type DataTableScopeFilter, ScopeBar } from "./scope-bar";
import {
  type DataTableView,
  type DataTableViewDeclaration,
  type DataTableViewState,
  declaredView,
  decodeView,
  encodeView,
} from "./view";
import { ViewBar } from "./view-bar";

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

/**
 * What a column is to its row: the row id, a reference to another record, the
 * row's revision, or any other value.
 */
export type DataTableColumnRole = "key" | "reference" | "revision" | "value";

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
  /** The column's role. A column that states none is a value. */
  readonly role?: DataTableColumnRole | undefined;
  /**
   * What a cell shows in place of its value, for example the text of the
   * record a key names. The sort, the filters, the search and the export
   * still read the value.
   */
  readonly cell?: ((value: unknown) => JSX.Element) | undefined;
}

export interface DataTableProps<TRow extends object> {
  /** The table's name, which the file name of a CSV export starts with. */
  readonly name: string;
  readonly columns: readonly DataTableColumn<TRow>[];
  /** The field that holds the id of each row, or null to number rows by position. */
  readonly rowId: (keyof TRow & string) | null;
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
  /** The declared scope filters: the fields the source can read by a list of values. */
  readonly scopeFilters: readonly (keyof TRow & string)[];
  /** Called with every scope filter that holds a value, after the scope bar changes one. */
  readonly onScopeChange: (filters: readonly DataTableScopeFilter<keyof TRow & string>[]) => void;
  /** The fields hidden when the table first draws. A view replaces them later. */
  readonly hiddenFields?: readonly (keyof TRow & string)[] | undefined;
  /** The fields grouped when the table first draws, in nesting order. A view replaces them later. */
  readonly groupedFields?: readonly (keyof TRow & string)[] | undefined;
  /** The time zone of a time's bucket. The default is the browser's zone. A setting replaces it later. */
  readonly timeZone?: string | undefined;
  /** The ISO weekday a week bucket starts on, 1 for Monday, the default. A setting replaces it later. */
  readonly weekStart?: number | undefined;
  /**
   * The key of the table's state in the URL, such as the definition name.
   * Only a top-level table has one. A table without one stays out of the URL.
   */
  readonly urlKey?: string | undefined;
  /** The buttons of one row, in a last column that does not sort, filter or search. */
  readonly rowActions?: ((row: TRow) => JSX.Element) | undefined;
}

/** The text the search box shows when the set is not fully read. */
const SEARCH_NEEDS_FULL_SET = "Search applies only to a fully read set.";

/** The label of the group of empty values. */
const NONE = "(none)";

/** True when a cell's shown text contains the search, in any case. */
const searchMatches = (value: unknown, search: string) =>
  value !== null && value !== undefined && String(value).toLowerCase().includes(search.toLowerCase());

/** The order of the group rows of each level, as the table reads it from its meta. */
interface GroupOrder {
  /** Changes whenever the order changes. */
  readonly key: string;
  readonly compare: (a: Row<any, any>, b: Row<any, any>) => number;
}

interface DataTableMeta {
  readonly groupOrder: () => GroupOrder | undefined;
}

/**
 * The grouped row model, with the groups of each level in the level's order.
 *
 * TanStack's sort orders every level by the table sort. A group row compares
 * equal under it, so the sort keeps the group order set here, by row index,
 * and orders only the rows inside the last level.
 */
function createOrderedGroupedRowModel() {
  const grouped = createGroupedRowModel();
  return (table: Table<any, any>) => {
    const model = grouped(table as never) as () => RowModel<any, any>;
    let last: { model: RowModel<any, any>; key: string; ordered: RowModel<any, any> } | undefined;
    return () => {
      const current = model();
      const order = (table.options.meta as DataTableMeta | undefined)?.groupOrder();
      if (order === undefined) {
        return current;
      }
      if (last !== undefined && last.model === current && last.key === order.key) {
        return last.ordered;
      }
      const level = (rows: Row<any, any>[]): Row<any, any>[] => {
        if (rows[0] === undefined || !rows[0].getIsGrouped()) {
          return rows;
        }
        const sorted = [...rows].sort(order.compare);
        sorted.forEach((row, index) => {
          (row as { index: number }).index = index;
          row.subRows = level(row.subRows);
        });
        return sorted;
      };
      const ordered = { ...current, rows: level(current.rows) };
      last = { model: current, key: order.key, ordered };
      return ordered;
    };
  };
}

/**
 * The feature bundle of every data table: the grid features, column and global
 * filtering, grouping and aggregation, and the filtered, grouped, sorted and
 * expanded row models. TanStack runs the row models in that order.
 */
const dataTableFeatures = tableFeatures({
  ...gridFeatures,
  columnFilteringFeature,
  // globalFilteringFeature requires columnFilteringFeature, declared above.
  globalFilteringFeature,
  columnGroupingFeature,
  rowAggregationFeature,
  filteredRowModel: createFilteredRowModel(),
  groupedRowModel: createOrderedGroupedRowModel(),
  sortedRowModel: createSortedRowModel(),
  expandedRowModel: createExpandedRowModel(),
  tableMeta: metaHelper<DataTableMeta>(),
});

export type DataTableFeatures = typeof dataTableFeatures;

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

/** The shown text of one value or aggregate. */
const shownText = (value: unknown): string =>
  value === null || value === undefined ? "" : String(value);

const groupable = (type: DataTableColumnType) => type !== "json" && type !== "bytes";

/** The data rows under a group row, without the group rows nested in it. */
const dataRows = <TRow extends object>(
  row: Row<DataTableFeatures, TRow>,
): Row<DataTableFeatures, TRow>[] => row.getLeafRows().filter((leaf) => !leaf.getIsGrouped());

/**
 * A group row's value, as its first data row gives it. TanStack keys a group
 * by the text of its value, so its own `groupingValue` holds "null" for the
 * group of empty values.
 */
function groupValue(row: object): unknown {
  // The group order and the cell type their rows differently; this names the bundle.
  const group = row as Row<DataTableFeatures, object>;
  return dataRows(group)[0]?.getGroupingValue(group.groupingColumnId!);
}

export function DataTable<TRow extends object>(props: DataTableProps<TRow>): JSX.Element {
  const timeZone = () => props.timeZone ?? Intl.DateTimeFormat().resolvedOptions().timeZone;
  const weekStart = () => props.weekStart ?? 1;
  const column = (field: string) => props.columns.find((candidate) => candidate.field === field)!;

  const [chosen, setChosen] = createSignal<Record<string, DataTableAggregate>>({});
  const aggregateOf = (field: string): DataTableAggregate => {
    const { type, role } = column(field);
    return chosen()[field] ?? defaultAggregate(type, role ?? "value");
  };
  const [buckets, setBuckets] = createSignal<Record<string, DataTableBucket>>({});
  const bucketFor = (field: string): DataTableBucket => buckets()[field] ?? "day";
  const [groupSorts, setGroupSorts] = createSignal<Record<string, DataTableGroupSort>>({});
  const groupSortFor = (field: string) => groupSorts()[field] ?? VALUE_SORT;

  // TanStack keeps each row's grouping values and each group's aggregates
  // once computed. A new bucket or aggregate hands it a new copy of the rows,
  // so it computes them again. Row ids and group ids stay the same.
  const [generation, setGeneration] = createSignal(0);
  const data = createMemo(() => {
    generation();
    return [...props.rows] as TRow[];
  });

  /** The scale of each numeric column: the most decimal places of any loaded value. */
  const scales = createMemo(() =>
    Object.fromEntries(
      props.columns
        .filter((candidate) => candidate.type === "numeric")
        .map((candidate) => [
          candidate.field,
          props.rows.reduce(
            (scale, row) =>
              isEmpty(row[candidate.field])
                ? scale
                : Math.max(scale, decimalScale(String(row[candidate.field]))),
            0,
          ),
        ]),
    ),
  );

  const aggregateRows = (field: string, rows: readonly Row<DataTableFeatures, TRow>[]) =>
    aggregateValues(
      column(field).type,
      aggregateOf(field),
      rows.map((row) => row.original[field as keyof TRow]),
      scales()[field] ?? 0,
    );

  // The column definitions change only with the columns and with the fully
  // read state. A getter that built them on every read would make the table
  // rebuild its columns on every read. TanStack copies each definition once,
  // so a getter in one would keep its first value.
  const columns = createMemo(() => {
    const fullyRead = props.fullyRead;
    const sortFields = props.sortFields;
    return [
      ...props.columns.map(
      (definition): ColumnDef<DataTableFeatures, TRow> => ({
        id: definition.field,
        header: (context) => (
          <div class="flex items-center gap-1">
            <SortHeader column={context.column} label={definition.label} onSort={sortChanged} />
            <ColumnFilter
              column={context.column}
              type={definition.type}
              label={definition.label}
              enabled={props.fullyRead}
            />
            <ColumnMenu
              column={context.column}
              label={definition.label}
              onSort={sortChanged}
              aggregates={allowedAggregates(definition.type)}
              aggregate={aggregateOf(definition.field)}
              onAggregate={(aggregate) => {
                setChosen((current) => ({ ...current, [definition.field]: aggregate }));
                setGeneration((value) => value + 1);
              }}
            />
          </div>
        ),
        cell: (context) => (
          <DataTableCell
            cell={context.cell}
            show={definition.cell}
            groupLabel={(field, value) =>
              column(field).type === "timestamptz"
                ? bucketLabel(String(value), bucketFor(field))
                : String(value)
            }
          />
        ),
        accessorFn: (row) => row[definition.field],
        // The starting width, before a drag sets one: a whole id or time fits.
        size: definition.type === "uuid" ? 300 : definition.type === "timestamptz" ? 240 : 150,
        sortFn: (a: Row<DataTableFeatures, TRow>, b: Row<DataTableFeatures, TRow>) =>
          // Group rows keep the order the group bar sets.
          a.getIsGrouped()
            ? 0
            : compareValues(definition.type, a.original[definition.field], b.original[definition.field]),
        filterFn: Object.assign(
          (row: Row<DataTableFeatures, TRow>, _id: string, filter: DataTableFilter) =>
            filterMatches(definition.type, row.original[definition.field], filter),
          { autoRemove: (filter: unknown) => filter === undefined },
        ),
        enableGlobalFilter: groupable(definition.type),
        enableGrouping: groupable(definition.type),
        // Every empty value joins one group, and a time groups by its bucket.
        getGroupingValue: (row) => {
          const value = row[definition.field];
          if (definition.type === "timestamptz") {
            return bucketOf(value, bucketFor(definition.field), timeZone(), weekStart());
          }
          return isEmpty(value) ? null : value;
        },
        aggregationFn: {
          aggregate: (context) =>
            aggregateRows(
              definition.field,
              context.groupingRow === undefined ? context.rows : dataRows(context.groupingRow),
            ),
        },
        // A set that is not fully read sorts only by a declared sort field.
        enableSorting: fullyRead || sortFields.some((sort) => sort.field === definition.field),
      }),
      ),
      ...(props.rowActions === undefined ? [] : [actionsColumn(props.rowActions)]),
    ];
  });

  /** The order of the groups of each level, as the group bar sets it. */
  const groupOrder = (): GroupOrder => {
    const grouping = table.atoms.grouping?.get() ?? [];
    const sorts = grouping.map(groupSortFor);
    return {
      key: JSON.stringify([sorts, chosen(), generation()]),
      compare: (a, b) => {
        const field = grouping[a.depth]!;
        const sort = sorts[a.depth]!;
        const order =
          sort.by === "value"
            ? compareValues(
                column(field).type === "timestamptz" ? "text" : column(field).type,
                groupValue(a),
                groupValue(b),
              )
            : compareAggregates(
                column(sort.by).type,
                aggregateOf(sort.by),
                a.getValue(sort.by) as AggregateResult,
                b.getValue(sort.by) as AggregateResult,
              );
        return sort.descending ? -order : order;
      },
    };
  };

  const table = createTable({
    features: dataTableFeatures,
    get data() {
      return data();
    },
    get columns() {
      return columns();
    },
    getRowId: (row, index) => (props.rowId === null ? String(index) : String(row[props.rowId])),
    meta: { groupOrder },
    manualPagination: true,
    get manualSorting() {
      return !props.fullyRead;
    },
    // A set that is not fully read keeps its filters, search and groups, and applies none.
    get manualFiltering() {
      return !props.fullyRead;
    },
    get manualGrouping() {
      return !props.fullyRead;
    },
    // A group stays expanded across a new load and a new grouping.
    autoResetExpanded: false,
    // TanStack searches hidden columns too, so the table limits it to visible ones.
    // The option types its column over any features, so it names this bundle.
    getColumnCanGlobalFilter: (column) =>
      (column as unknown as Column<DataTableFeatures, TRow, unknown>).getIsVisible(),
    globalFilterFn: (row: Row<DataTableFeatures, TRow>, columnId: string, search: string) =>
      searchMatches(row.getValue(columnId), search),
    initialState: {
      columnVisibility: Object.fromEntries((props.hiddenFields ?? []).map((field) => [field, false])),
      grouping: [...(props.groupedFields ?? [])],
    },
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

  /** After a header or menu sort: a set that is not fully read asks for a new load. */
  function sortChanged() {
    if (!props.fullyRead) {
      props.onSortChange(
        (table.atoms.sorting?.get() ?? []).map((sort) => ({
          field: sort.id as keyof TRow & string,
          direction: sort.desc ? "descending" : "ascending",
        })),
      );
    }
  }

  // TanStack filters again only when the data or the filters change, and the
  // search reads the visible columns, so a change of visibility filters again
  // while a search is active. A new copy of the rows also groups them again.
  createEffect(
    on(
      () => table.atoms.columnVisibility?.get(),
      () => {
        if (untrack(search) !== "") {
          setGeneration((value) => value + 1);
        }
      },
      { defer: true },
    ),
  );

  /** The values of each scope filter. The source reads them, and the table applies none. */
  const [scope, setScope] = createSignal<Record<string, readonly string[]>>({});
  function changeScope(field: string, values: readonly string[]) {
    const next = { ...scope(), [field]: values };
    setScope(next);
    props.onScopeChange(
      props.scopeFilters
        .filter((declared) => (next[declared] ?? []).length > 0)
        .map((declared) => ({ field: declared, values: next[declared]! })),
    );
  }

  /** The current sort, as the scope bar shows it. */
  const sortChips = () =>
    (table.atoms.sorting?.get() ?? []).map(
      (sort) => `${column(sort.id).label} ${sort.desc ? "descending" : "ascending"}`,
    );

  /** Every column in the table's order, as the column panel lists it. */
  const panelColumns = () => {
    table.atoms.columnVisibility?.get();
    const order = table.atoms.columnOrder?.get() ?? [];
    const fields = props.columns.map((candidate) => candidate.field as string);
    return [...order.filter((id) => fields.includes(id)), ...fields.filter((id) => !order.includes(id))].map(
      (id) => ({ id, label: column(id).label, visible: table.getColumn(id)?.getIsVisible() ?? true }),
    );
  };

  const search = () => (table.atoms.globalFilter?.get() as string | undefined) ?? "";

  /** What the table declares, which a view is checked against. */
  const declaration = (): DataTableViewDeclaration => ({
    columns: props.columns.map((declared) => ({
      field: declared.field,
      time: declared.type === "timestamptz",
      groupable: groupable(declared.type),
      aggregates: allowedAggregates(declared.type),
    })),
    scopeFilters: props.scopeFilters,
  });

  /** The state of the table definition, which reset applies. The cap is the first one. */
  const defaults: DataTableViewState = untrack(() => ({
    order: props.columns.map((declared) => declared.field as string),
    hidden: [...(props.hiddenFields ?? [])],
    widths: {},
    left: [],
    right: [],
    sort: [],
    scope: [],
    filters: [],
    search: "",
    group: (props.groupedFields ?? []).map((field) => ({
      field,
      bucket: column(field).type === "timestamptz" ? "day" : null,
      sort: VALUE_SORT,
    })),
    aggregates: {},
    cap: props.cap,
  }));

  /** The table state as a view holds it. */
  const currentView = (): DataTableViewState => {
    const visibility = table.atoms.columnVisibility?.get() ?? {};
    const pinning = table.atoms.columnPinning?.get();
    return {
      order: panelColumns().map((shown) => shown.id),
      hidden: props.columns.map((shown) => shown.field as string).filter((field) => visibility[field] === false),
      widths: Object.fromEntries(
        Object.entries(table.atoms.columnSizing?.get() ?? {}).map(([field, width]) => [field, Math.round(width)]),
      ),
      left: [...(pinning?.start ?? [])],
      right: [...(pinning?.end ?? [])],
      sort: (table.atoms.sorting?.get() ?? []).map((sort) => ({
        field: sort.id,
        direction: sort.desc ? "descending" : "ascending",
      })),
      scope: props.scopeFilters
        .filter((field) => (scope()[field] ?? []).length > 0)
        .map((field) => ({ field, values: scope()[field]! })),
      filters: (table.atoms.columnFilters?.get() ?? []).map((active) => ({
        field: active.id,
        filter: active.value as DataTableFilter,
      })),
      search: search(),
      group: grouping().map((field) => ({
        field,
        bucket: column(field).type === "timestamptz" ? bucketFor(field) : null,
        sort: groupSortFor(field),
      })),
      aggregates: Object.fromEntries(
        Object.entries(chosen()).filter(
          ([field, aggregate]) => aggregate !== defaultAggregate(column(field).type, column(field).role ?? "value"),
        ),
      ),
      cap: props.cap,
    };
  };

  /**
   * Sets the table to a view, without the fields the table does not declare.
   * The scope and the cap reach the source, which reads again. A sort reaches
   * it too when the set is not fully read.
   */
  function applyView(state: DataTableViewState) {
    const view = declaredView(state, declaration());
    const sortBefore = JSON.stringify(table.atoms.sorting?.get() ?? []);
    table.setColumnOrder([...view.order]);
    table.setColumnVisibility(
      Object.fromEntries(props.columns.map((shown) => [shown.field, !view.hidden.includes(shown.field)])),
    );
    table.setColumnSizing({ ...view.widths });
    table.setColumnPinning({ start: [...view.left], end: [...view.right] });
    table.setSorting(view.sort.map((sort) => ({ id: sort.field, desc: sort.direction === "descending" })));
    table.setColumnFilters(view.filters.map((active) => ({ id: active.field, value: active.filter })));
    table.setGlobalFilter(view.search === "" ? undefined : view.search);
    table.setGrouping(view.group.map((level) => level.field));
    setBuckets(
      Object.fromEntries(view.group.flatMap((level) => (level.bucket === null ? [] : [[level.field, level.bucket]]))),
    );
    setGroupSorts(Object.fromEntries(view.group.map((level) => [level.field, level.sort])));
    setChosen({ ...view.aggregates });
    setGeneration((value) => value + 1);
    if (JSON.stringify(currentView().scope) !== JSON.stringify(view.scope)) {
      setScope(Object.fromEntries(view.scope.map((scoped) => [scoped.field, scoped.values])));
      props.onScopeChange(view.scope as readonly DataTableScopeFilter<keyof TRow & string>[]);
    }
    if (view.cap !== props.cap) {
      props.onCapChange(view.cap);
    }
    if (JSON.stringify(table.atoms.sorting?.get() ?? []) !== sortBefore) {
      sortChanged();
    }
  }

  const [views, setViews] = createSignal<readonly DataTableView[]>([]);
  const [chosenView, setChosenView] = createSignal<string | null>(null);

  // A table with a URL key reads its state from the URL once, and then keeps the URL in step.
  const [ignored, setIgnored] = createSignal<readonly string[]>([]);
  onMount(() => {
    const key = props.urlKey;
    if (key === undefined) {
      return;
    }
    const read = decodeView(key, new URLSearchParams(window.location.search), defaults, declaration());
    setIgnored(read.ignored);
    applyView(read.state);
    createEffect(() => {
      // The other keys stay as they were written, and this table's keys follow them.
      const others = window.location.search
        .slice(1)
        .split("&")
        .filter((part) => part !== "" && !part.startsWith(`${encodeURIComponent(key)}.`));
      const own = new URLSearchParams(encodeView(key, currentView(), defaults)).toString();
      const query = [...others, ...(own === "" ? [] : [own])].join("&");
      window.history.replaceState(
        window.history.state,
        "",
        `${window.location.pathname}${query === "" ? "" : `?${query}`}${window.location.hash}`,
      );
    });
  });
  const searchId = createUniqueId();
  const grouping = () => table.atoms.grouping?.get() ?? [];

  /** The active filters, in the order they were set, as their chips name them. */
  const filters = createMemo(() =>
    (table.atoms.columnFilters?.get() ?? []).flatMap((active) => {
      const filtered = props.columns.find((candidate) => candidate.field === active.id);
      return filtered === undefined
        ? []
        : [
            {
              id: active.id,
              label: filtered.label,
              text: filterText(filtered.type, active.value as DataTableFilter),
            },
          ];
    }),
  );

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

  /** The visible columns that hold a field, which leaves out the row buttons. */
  const dataColumns = () =>
    table.getVisibleLeafColumns().filter((leaf) => leaf.id !== ACTIONS_COLUMN);

  /** The footer: each visible column's aggregate over every kept row. */
  const totals = createMemo(() => {
    const rows = table.getFilteredRowModel().rows;
    return dataColumns().map((visible) => ({
      id: visible.id,
      aggregate: aggregateOf(visible.id),
      value: aggregateRows(visible.id, rows),
    }));
  });

  /**
   * Saves the kept rows as CSV: the visible columns in their order, and the
   * data rows in the table sort, without group rows or totals.
   */
  function exportCsv() {
    const visible = dataColumns().map((leaf) => column(leaf.id));
    const sorting = table.atoms.sorting?.get() ?? [];
    const rows = [...table.getFilteredRowModel().rows].sort((a, b) => {
      for (const sort of sorting) {
        const field = column(sort.id).field;
        const order = compareValues(column(sort.id).type, a.original[field], b.original[field]);
        if (order !== 0) {
          return sort.desc ? -order : order;
        }
      }
      return 0;
    });
    const text = csvText([
      visible.map((shown) => shown.label),
      ...rows.map((row) => visible.map((shown) => shownText(row.original[shown.field]))),
    ]);
    downloadCsv(csvFileName(props.name, new Date()), text);
  }

  return (
    <section data-slot="data-table" class="flex h-full min-h-0 min-w-0 flex-col gap-4">
      <Show when={props.scopeFilters.length > 0 || sortChips().length > 0}>
        <ScopeBar
          filters={props.scopeFilters.map((field) => ({
            field,
            label: column(field).label,
            values: scope()[field] ?? [],
          }))}
          sort={sortChips()}
          onChange={changeScope}
        />
      </Show>
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
          <Field class="w-auto">
            <Button type="button" variant="outline" disabled={!props.fullyRead} onClick={exportCsv}>
              export CSV
            </Button>
            <Show when={!props.fullyRead}>
              <FieldDescription>{EXPORT_NEEDS_FULL_SET}</FieldDescription>
            </Show>
          </Field>
          <ViewBar
            names={views().map((view) => view.name)}
            chosen={chosenView()}
            ignored={ignored()}
            onPick={(name) => {
              const view = views().find((candidate) => candidate.name === name);
              if (view !== undefined) {
                applyView(view.state);
                setChosenView(name);
              }
            }}
            onSave={(name) => {
              const state = currentView();
              setViews((current) =>
                current.some((view) => view.name === name)
                  ? current.map((view) => (view.name === name ? { name, state } : view))
                  : [...current, { name, state }],
              );
              setChosenView(name);
            }}
            onRename={(from, to) => {
              setViews((current) => current.map((view) => (view.name === from ? { ...view, name: to } : view)));
              setChosenView(to);
            }}
            onDelete={(name) => {
              setViews((current) => current.filter((view) => view.name !== name));
              setChosenView(null);
            }}
            onReset={() => {
              applyView(defaults);
              setChosenView(null);
            }}
          />
          <ColumnPanel
            columns={panelColumns()}
            onVisible={(id, visible) => table.getColumn(id)?.toggleVisibility(visible)}
            onOrder={(order) => table.setColumnOrder([...order])}
            onShowAll={() =>
              table.setColumnVisibility(Object.fromEntries(props.columns.map((shown) => [shown.field, true])))
            }
            onReset={() => table.setColumnOrder(props.columns.map((shown) => shown.field as string))}
          />
          <Field class="w-64">
            <FieldLabel for={searchId}>search</FieldLabel>
            <Input
              id={searchId}
              type="search"
              value={search()}
              disabled={!props.fullyRead}
              onInput={(event) =>
                table.setGlobalFilter(
                  event.currentTarget.value === "" ? undefined : event.currentTarget.value,
                )
              }
            />
            <Show when={!props.fullyRead}>
              <FieldDescription>{SEARCH_NEEDS_FULL_SET}</FieldDescription>
            </Show>
          </Field>
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
        <Show when={filters().length > 0}>
          <div data-slot="data-table-filters" class="flex w-full flex-wrap items-center gap-2">
            <For each={filters()}>
              {(filter) => (
                <Badge variant="outline" class="gap-1" aria-disabled={!props.fullyRead}>
                  {filter.label} {filter.text}
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon-xs"
                    aria-label={`remove filter ${filter.label}`}
                    disabled={!props.fullyRead}
                    onClick={() => table.getColumn(filter.id)?.setFilterValue(undefined)}
                  >
                    <X aria-hidden="true" />
                  </Button>
                </Badge>
              )}
            </For>
            <Button
              type="button"
              variant="ghost"
              size="sm"
              disabled={!props.fullyRead}
              onClick={() => table.setColumnFilters([])}
            >
              clear all
            </Button>
            <Show when={!props.fullyRead}>
              <p class="text-sm text-muted-foreground">{FILTER_NEEDS_FULL_SET}</p>
            </Show>
          </div>
        </Show>
      </div>
      <GroupBar
        columns={props.columns.map((candidate) => ({
          field: candidate.field,
          label: candidate.label,
          time: candidate.type === "timestamptz",
          groupable: groupable(candidate.type),
          aggregate: aggregateOf(candidate.field),
        }))}
        grouping={grouping()}
        bucket={bucketFor}
        sort={groupSortFor}
        enabled={props.fullyRead}
        onGrouping={(next) => table.setGrouping([...next])}
        onBucket={(field, bucket) => {
          setBuckets((current) => ({ ...current, [field]: bucket }));
          setGeneration((value) => value + 1);
        }}
        onSort={(field, sort) => setGroupSorts((current) => ({ ...current, [field]: sort }))}
        onExpandLevel={expandLevel}
      />
      <DataGrid
        table={table}
        recordCount={props.rows.length}
        isLoading={props.busy}
        // A header edge drag sets the width, which the table state keeps. A pinned column sticks.
        // The resize mode is a table option: a tableLayout mode would reset the options, which
        // reads each getter above once and keeps its value.
        tableLayout={{ columnsResizable: true, columnsPinnable: true }}
        emptyMessage={
          props.refusal ??
          (filters().length > 0 || search() !== "" ? "No row matches the search and filters." : null)
        }
      >
        {/* The grid takes the height the toolbar leaves, in place of its fixed one. */}
        <DataGridContainer class="h-auto min-h-0 flex-1">
          <WindowedTable
            footerContent={
              <Show when={props.fullyRead && props.rows.length > 0}>
                <DataGridTableFootRow>
                  <For each={totals()}>
                    {(total) => (
                      <DataGridTableFootRowCell>
                        <span data-slot="data-table-total" data-field={total.id}>
                          <span class="text-muted-foreground">{total.aggregate}</span>{" "}
                          {shownText(total.value)}
                        </span>
                      </DataGridTableFootRowCell>
                    )}
                  </For>
                </DataGridTableFootRow>
              </Show>
            }
          />
        </DataGridContainer>
      </DataGrid>
    </section>
  );
}

/**
 * One cell: a group's value, row count and expand control, a group's
 * aggregate, nothing under a group's own column, or a row's value.
 */
function DataTableCell<TRow extends object>(props: {
  cell: Cell<DataTableFeatures, TRow, unknown>;
  /** The label of a group of one column with a non-empty value. */
  groupLabel: (field: string, value: unknown) => string;
  /** What a data row's cell shows in place of its value. */
  show?: ((value: unknown) => JSX.Element) | undefined;
}): JSX.Element {
  const row = () => props.cell.row;
  const label = () => {
    const value = groupValue(row());
    return isEmpty(value) ? NONE : props.groupLabel(props.cell.column.id, value);
  };
  return (
    <Switch fallback={props.show?.(props.cell.getValue()) ?? shownText(props.cell.getValue())}>
      <Match when={props.cell.getIsGrouped()}>
        <Button
          type="button"
          variant="ghost"
          size="sm"
          class="-ms-2"
          aria-expanded={row().getIsExpanded()}
          onClick={() => row().toggleExpanded()}
        >
          <Show when={row().getIsExpanded()} fallback={<ChevronRight aria-hidden="true" />}>
            <ChevronDown aria-hidden="true" />
          </Show>
          <span data-slot="data-table-group-value">{label()}</span>
          <span class="text-muted-foreground">({dataRows(row()).length})</span>
        </Button>
      </Match>
      <Match when={props.cell.getIsPlaceholder()}>{""}</Match>
      <Match when={props.cell.getIsAggregated()}>{shownText(props.cell.getValue())}</Match>
    </Switch>
  );
}

/** The id of the last column, which holds the buttons of each row. */
const ACTIONS_COLUMN = "rowActions";

/** The last column: the buttons of each data row, and nothing on a group row. */
function actionsColumn<TRow extends object>(
  actions: (row: TRow) => JSX.Element,
): ColumnDef<DataTableFeatures, TRow> {
  return {
    id: ACTIONS_COLUMN,
    header: "",
    cell: (cell) => (
      <Show when={!cell.row.getIsGrouped()}>
        <div class="flex gap-2">{actions(cell.row.original)}</div>
      </Show>
    ),
    enableSorting: false,
    enableColumnFilter: false,
    enableGlobalFilter: false,
    enableGrouping: false,
    enableHiding: false,
    enablePinning: false,
    enableResizing: false,
  };
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
