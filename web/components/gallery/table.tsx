/**
 * The DataTable over the load state of `@wamn/web-runtime`.
 *
 * The seam states render the fixture's emitted table definition for the widget
 * query and read it through a stub transport. The stub refuses a limit above
 * the page maximum, as the server does, so a load reads one page of at most
 * that many rows. The stub sorts by the sort of the request. A set of 3 rows is
 * fully read, so the table sorts it. A set of 150 rows is not, so a sort starts
 * a new load in the new order. The 1000 and 10,000 row states generate
 * their rows in memory, because one page cannot hold them. Both sources hand
 * their result to the same load state.
 */

import { createSignal, For, type JSX } from "solid-js";

import { DataTable, type DataTableColumn, type DataTableSort } from "@wamn/ui";
import {
  emptyLoad,
  finishLoad,
  loadLimit,
  startLoad,
  type LoadPage,
  type LoadState,
  type Outcome,
  type Transport,
} from "@wamn/web-runtime";

import { WIDGET_QUERY_TABLE } from "../fixture/components/widget.js";
import { query, type WidgetQueryRequestSort, type WidgetQueryRow } from "../fixture/widget.js";
import { Section, State } from "./section.js";

/** The page maximum the fixture contract declares. */
const PAGE_MAXIMUM = WIDGET_QUERY_TABLE.pageMaximum;

/** The default cap of the platform table. */
const DEFAULT_CAP = 1000;

/**
 * A transport that answers the widget query from a set of `size` widgets, in
 * id order or in the order of the request's sort. The creation times do not
 * follow the ids, so a sort changes the order.
 */
function widgetStub(size: number): Transport {
  const widgets = Array.from({ length: size }, (_, index) => ({
    id: `00000000-0000-4000-8000-${String(index).padStart(12, "0")}`,
    code: index % 3 === 0 ? "priority" : "standard",
    note: null,
    edit_version: "1",
    created_at: `2026-09-21T${String((index * 7) % 24).padStart(2, "0")}:${String(index % 60).padStart(2, "0")}:00.000000Z`,
  }));
  return {
    invoke: (request) => {
      const item = request.items[0] as
        | { readonly limit?: number; readonly sort?: { field: string; direction: string } }
        | undefined;
      const limit = item?.limit ?? PAGE_MAXIMUM;
      if (limit < 1 || limit > PAGE_MAXIMUM) {
        return Promise.resolve({ status: "refused", code: "invalid_input", detail: null });
      }
      const sign = item?.sort?.direction === "descending" ? -1 : 1;
      const ordered =
        item?.sort === undefined
          ? widgets
          : [...widgets].sort((a, b) => sign * a.created_at.localeCompare(b.created_at));
      return Promise.resolve({
        status: "completed",
        value: { item: ordered.slice(0, limit), next_cursor: size > limit ? "more" : null },
      });
    },
  };
}

/** One table that renders the widget query's definition and loads it through the stub. */
function SeamTable(props: { size: number }): JSX.Element {
  const definition = WIDGET_QUERY_TABLE;
  const transport = widgetStub(props.size);
  const [state, setState] = createSignal<LoadState<WidgetQueryRow>>(emptyLoad(DEFAULT_CAP));
  const [sort, setSort] = createSignal<WidgetQueryRequestSort>();

  async function load(cap: number) {
    const next = startLoad(state(), cap);
    setState(next);
    const order = sort();
    const outcome = await query(transport, [
      {
        limit: loadLimit(next.cap, definition.pageMaximum),
        ...(order === undefined ? {} : { sort: order }),
      },
    ]);
    setState((current) => finishLoad(current, next.generation, outcome, definition.rowId));
  }
  void load(DEFAULT_CAP);

  // The table names the field, and the request sends its wire name. The
  // contract sorts by one field, so the sort holds one.
  function sortBy(sorts: readonly DataTableSort<WidgetQueryRow>[]) {
    const [first] = sorts;
    const declared = definition.sortFields.find((sort) => sort.field === first?.field);
    if (first !== undefined && declared !== undefined) {
      setSort({ field: declared.wire, direction: first.direction });
      void load(state().cap);
    }
  }

  return (
    <LoadedTable
      columns={definition.columns}
      rowId={definition.rowId}
      state={state()}
      load={load}
      sortFields={definition.sortFields}
      sortMaxFields={definition.sortMaxFields}
      onSortChange={sortBy}
    />
  );
}

/** One generated pallet row. */
interface PalletRow {
  readonly id: string;
  readonly code: string;
  readonly quantity: number;
  readonly weight: string;
  readonly createdAt: string;
}

const PALLET_COLUMNS: readonly DataTableColumn<PalletRow>[] = [
  { field: "code", label: "code", type: "text" },
  { field: "quantity", label: "quantity", type: "int32" },
  { field: "weight", label: "weight", type: "numeric" },
  { field: "createdAt", label: "created at", type: "timestamptz" },
  { field: "id", label: "id", type: "uuid" },
];

function pallets(count: number): PalletRow[] {
  return Array.from({ length: count }, (_, index) => ({
    id: `00000000-0000-4000-8000-${String(index).padStart(12, "0")}`,
    code: `P-${String(index + 1).padStart(5, "0")}`,
    quantity: 1 + (index % 48),
    weight: (100 + (index % 900) / 4).toFixed(2),
    createdAt: `2026-09-${String(1 + (index % 28)).padStart(2, "0")}T12:00:00.000000Z`,
  }));
}

/** One table over a set of `size` rows generated in memory, loaded up to its cap. */
function MemoryTable(props: { size: number; cap?: number }): JSX.Element {
  const [state, setState] = createSignal<LoadState<PalletRow>>(
    emptyLoad(props.cap ?? DEFAULT_CAP),
  );

  function load(cap: number) {
    const next = startLoad(state(), cap);
    setState(next);
    const outcome: Outcome<LoadPage<PalletRow>> = {
      status: "completed",
      value: {
        item: pallets(Math.min(props.size, next.cap)),
        nextCursor: props.size > next.cap ? "more" : null,
      },
    };
    setState((current) => finishLoad(current, next.generation, outcome, "id"));
  }
  load(state().cap);

  // No source sorts these rows, so only a fully read set sorts, in the table.
  // Two sort fields let a shift click show a sort by more than one field.
  return (
    <LoadedTable
      columns={PALLET_COLUMNS}
      rowId="id"
      state={state()}
      load={load}
      sortFields={[]}
      sortMaxFields={2}
      onSortChange={() => {}}
    />
  );
}

/** The DataTable over one load state. A refresh loads again at the cap in force. */
function LoadedTable<Row extends object>(props: {
  columns: readonly DataTableColumn<Row>[];
  rowId: keyof Row & string;
  state: LoadState<Row>;
  load: (cap: number) => void;
  sortFields: readonly { readonly field: keyof Row & string }[];
  sortMaxFields: number;
  onSortChange: (sort: readonly DataTableSort<Row>[]) => void;
}): JSX.Element {
  return (
    <DataTable
      columns={props.columns}
      rowId={props.rowId}
      rows={props.state.rows}
      fullyRead={props.state.fullyRead}
      busy={props.state.busy}
      refusal={props.state.refusal}
      cap={props.state.cap}
      onCapChange={props.load}
      onRefresh={() => props.load(props.state.cap)}
      startedAt={props.state.startedAt}
      endedAt={props.state.endedAt}
      sortFields={props.sortFields}
      sortMaxFields={props.sortMaxFields}
      onSortChange={props.onSortChange}
    />
  );
}

/**
 * One 10,000 row table at the full height of the viewport, under a header:
 * the shape an app gives a table. The app sizes the box, and the table fills
 * it, so only the grid body scrolls.
 */
export function AppTable(): JSX.Element {
  return (
    <div class="flex h-screen flex-col">
      <header class="flex h-14 shrink-0 items-center border-b px-6">
        <p class="text-sm font-semibold uppercase">App header</p>
      </header>
      <main class="min-h-0 flex-1 p-6">
        <MemoryTable size={10000} cap={10000} />
      </main>
    </div>
  );
}

/** The query string that serves the app-shaped table alone, for measurement. */
export const APP_TABLE_ONLY = "app-table";

export function TableSections(): JSX.Element {
  return (
    <>
      <Section title="Data table" name="DataTable, LoadState">
        <div class="flex flex-col gap-6">
          <State name="WIDGET_QUERY_TABLE, fully read: a header click sorts in the table">
            <div class="h-[32rem]">
              <SeamTable size={3} />
            </div>
          </State>
          <State name="WIDGET_QUERY_TABLE, not fully read: a sort by created at loads again in that order">
            <div class="h-[32rem]">
              <SeamTable size={150} />
            </div>
          </State>
          <For each={[1000, 10000]}>
            {(size) => (
              <State name={`${size} rows generated in memory`}>
                <div class="h-[32rem]">
                  <MemoryTable size={size} />
                </div>
              </State>
            )}
          </For>
        </div>
      </Section>
      <Section title="Data table in an app" name="DataTable">
        <State name={`10000 rows at the full viewport height; served alone at ?${APP_TABLE_ONLY}`}>
          <AppTable />
        </State>
      </Section>
    </>
  );
}
