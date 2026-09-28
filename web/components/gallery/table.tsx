/**
 * The platform tables: a QueryTable over a read, and a SetTable over a set.
 *
 * The QueryTable states render the fixture's emitted table definition for the
 * widget query and read it through a stub transport. The stub refuses a limit
 * above the page maximum, as the server does, so a load reads one page of at
 * most that many rows. The stub keeps the codes of the request's scope filter
 * and sorts by the request's sort. A scope change in the table reads again. A
 * set of 3 rows is fully read, so a set table shows it and sorts it. A set of
 * 150 rows is not, so a sort starts a new load in the new order.
 *
 * The SetTable states hold their rows in memory, 100, 1000 and 10,000 of
 * them, and their own state. The grouped state reads its 10,000 rows through a
 * QueryTable over a memory read, which keeps its state in the URL.
 */

import { createSignal, For, type JSX } from "solid-js";

import {
  builtColumns,
  defaultGridView,
  defaultSetView,
  QueryTable,
  type QueryTableDefinition,
  SetTable,
  type TableColumn,
  type TableSort,
} from "@wamn/ui";
import type { Transport } from "@wamn/web-runtime";

import { WIDGET_QUERY_TABLE } from "../fixture/components/widget.js";
import { WIDGET_MAKER_QUERY_TABLE } from "../fixture/components/widget_maker.js";
import type { WidgetQueryRow } from "../fixture/widget.js";
import { memoryDefinition, memoryTransport } from "./memory.js";
import { Section, State } from "./section.js";
import { ActionTable } from "./table-actions.js";

/** The page maximum the fixture contract declares. */
const PAGE_MAXIMUM = WIDGET_QUERY_TABLE.pageMaximum!;

/** The widget table with its scope and sort alone: no edit, no row buttons, and the code as its one scope filter. */
const { update: _update, ...WIDGET_READ } = WIDGET_QUERY_TABLE;
const WIDGETS: QueryTableDefinition<WidgetQueryRow> = {
  ...WIDGET_READ,
  scopeFilters: ["code"],
  actions: [],
  childTables: [],
};

/**
 * A transport that answers the widget query from a set of `size` widgets, in
 * id order or in the order of the request's sort. The creation times do not
 * follow the ids, so a sort changes the order.
 */
function widgetStub(size: number): Transport {
  const widgets = Array.from({ length: size }, (_, index) => ({
    id: `00000000-0000-4000-8000-${String(index).padStart(12, "0")}`,
    code: index % 3 === 0 ? "priority" : "standard",
    maker_id: null,
    note: null,
    edit_version: "1",
    created_at: `2026-09-21T${String((index * 7) % 24).padStart(2, "0")}:${String(index % 60).padStart(2, "0")}:00.000000Z`,
  }));
  return {
    invoke: (request) => {
      const item = request.items[0] as
        | {
            readonly limit?: number;
            readonly filter?: { readonly code?: readonly string[] };
            readonly sort?: { field: string; direction: string };
          }
        | undefined;
      const limit = item?.limit ?? PAGE_MAXIMUM;
      if (limit < 1 || limit > PAGE_MAXIMUM) {
        return Promise.resolve({ status: "refused", code: "invalid_input", detail: null });
      }
      const codes = item?.filter?.code;
      const kept = codes === undefined ? widgets : widgets.filter((widget) => codes.includes(widget.code));
      const sign = item?.sort?.direction === "descending" ? -1 : 1;
      const ordered =
        item?.sort === undefined ? kept : [...kept].sort((a, b) => sign * a.created_at.localeCompare(b.created_at));
      return Promise.resolve({
        status: "completed",
        value: { item: ordered.slice(0, limit), next_cursor: ordered.length > limit ? "more" : null },
      });
    },
  };
}

/** One generated pallet row. */
interface PalletRow {
  readonly id: string;
  readonly code: string;
  readonly quantity: number;
  readonly weight: string;
  readonly createdAt: string;
}

const PALLET_COLUMNS: readonly TableColumn<PalletRow>[] = [
  { field: "code", label: "code", type: "text" },
  { field: "quantity", label: "quantity", type: "int32" },
  { field: "weight", label: "weight", type: "numeric" },
  { field: "createdAt", label: "created at", type: "timestamptz" },
  { field: "id", label: "id", type: "uuid", role: "key" },
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

/**
 * A set table over `size` pallets in memory, with its own state. Two sort
 * fields let a shift click show a sort by more than one field.
 */
export function MemoryTable(props: { size: number }): JSX.Element {
  const rows = pallets(props.size);
  const [view, setView] = createSignal(defaultSetView());
  const [grid, setGrid] = createSignal(defaultGridView(PALLET_COLUMNS.map((column) => column.field)));
  const [sort, setSort] = createSignal<readonly TableSort[]>([]);
  return (
    <div class="flex h-full min-h-0 flex-col gap-4">
      <SetTable
        name="pallet"
        rows={rows}
        rowId={(row) => row.id}
        columns={builtColumns(PALLET_COLUMNS)}
        view={view()}
        onView={setView}
        grid={grid()}
        onGrid={setGrid}
        sort={sort()}
        onSort={setSort}
        sortMaxFields={2}
        gridClass="h-auto min-h-0 flex-1"
      />
    </div>
  );
}

/** The 10,000 pallets through a QueryTable, grouped, with the table state in the URL. */
function GroupedPallets(): JSX.Element {
  const rows = pallets(10000);
  return (
    <QueryTable
      definition={memoryDefinition("pallet", PALLET_COLUMNS, { sortMaxFields: 2 })}
      transport={memoryTransport(() => rows)}
      label="pallets"
      urlKey="pallets"
      groupedFields={["createdAt", "quantity"]}
    />
  );
}

/**
 * The maker table from its emitted definition, over a stub of three makers.
 * Its scope bar shows the required band of the last 30 days, the contains
 * filter on the name, and the server search.
 */
function MakerTable(): JSX.Element {
  const makers = ["Acme", "Globex", "Initech"].map((name, index) => ({
    id: `00000000-0000-4000-8000-00000000000${index}`,
    name,
    edit_version: "1",
    created_at: `2026-09-2${index}T12:00:00.000000Z`,
  }));
  const transport: Transport = {
    invoke: () => Promise.resolve({ status: "completed", value: { item: makers, next_cursor: null } }),
  };
  return <QueryTable definition={WIDGET_MAKER_QUERY_TABLE} transport={transport} label="makers" />;
}

/**
 * One table at the full height of the viewport, under a header: the shape an
 * app gives a table. The app sizes the box, and the table fills it, so only
 * the grid body scrolls. It is the widget table with its row buttons, its bulk
 * action, its editable cells and its child table, over one stub transport.
 */
export function AppTable(): JSX.Element {
  return (
    <div class="flex h-screen flex-col">
      <header class="flex h-14 shrink-0 items-center border-b px-6">
        <p class="text-sm font-semibold uppercase">App header</p>
      </header>
      <main class="min-h-0 flex-1 p-6">
        <ActionTable size={60} />
      </main>
    </div>
  );
}

/** The query string that serves the app-shaped table alone, for measurement. */
export const APP_TABLE_ONLY = "app-table";

export function TableSections(): JSX.Element {
  return (
    <>
      <Section title="Query table" name="QueryTable">
        <div class="flex flex-col gap-6">
          <State name="WIDGET_QUERY_TABLE, fully read: a set table shows the rows, and a header click sorts them">
            <div class="h-[32rem]">
              <QueryTable definition={WIDGETS} transport={widgetStub(3)} label="widgets" />
            </div>
          </State>
          <State name="WIDGET_QUERY_TABLE, not fully read: a sort by created at loads again in that order">
            <div class="h-[32rem]">
              <QueryTable definition={WIDGETS} transport={widgetStub(150)} label="widgets" />
            </div>
          </State>
          <State name="WIDGET_MAKER_QUERY_TABLE: a required band of the last 30 days, a contains filter and a server search in the scope bar">
            <div class="h-[32rem]">
              <MakerTable />
            </div>
          </State>
          <State name="a first load in progress">
            <div class="h-[32rem]">
              <QueryTable
                definition={memoryDefinition("pallet", PALLET_COLUMNS)}
                transport={memoryTransport<PalletRow>(() => [], { never: true })}
                label="pallets"
              />
            </div>
          </State>
          <State name="10000 rows grouped by the day of creation, then by quantity, with the table state in the URL">
            <div class="h-[32rem]">
              {/* The one table that keeps its state in the URL, so a reload brings it back. */}
              <GroupedPallets />
            </div>
          </State>
        </div>
      </Section>
      <Section title="Set table" name="SetTable">
        <div class="flex flex-col gap-6">
          <For each={[0, 100, 1000, 10000]}>
            {(size) => (
              <State name={size === 0 ? "no rows" : `${size} rows in memory`}>
                <div class="h-[32rem]">
                  <MemoryTable size={size} />
                </div>
              </State>
            )}
          </For>
        </div>
      </Section>
      <Section title="Query table in an app" name="QueryTable in an app">
        <State
          name={`the widget table with row buttons, a bulk action, editable cells and a child table; served alone at ?${APP_TABLE_ONLY}`}
        >
          <AppTable />
        </State>
      </Section>
    </>
  );
}
