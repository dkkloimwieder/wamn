/**
 * The DataTable over the load state of `@wamn/web-runtime`.
 *
 * The seam states render the fixture's emitted table definition for the widget
 * query and read it through a stub transport, at 3 and 100 rows. The stub
 * refuses a limit above the page maximum, as the server does, so a load reads
 * one page of at most that many rows. The 1000 and 10,000 row states generate
 * their rows in memory, because one page cannot hold them. Both sources hand
 * their result to the same load state.
 */

import { createSignal, For, type JSX } from "solid-js";

import { DataTable, type DataTableColumn } from "@wamn/ui";
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
import { query, type WidgetQueryRow } from "../fixture/widget.js";
import { page } from "../stubs/index.js";
import { Section, State } from "./section.js";

/** The page maximum the fixture contract declares. */
const PAGE_MAXIMUM = WIDGET_QUERY_TABLE.pageMaximum;

/** The default cap of the platform table. */
const DEFAULT_CAP = 1000;

/** A transport that answers the widget query from a set of `size` widgets. */
function widgetStub(size: number): Transport {
  const ids = Array.from(
    { length: size },
    (_, index) => `00000000-0000-4000-8000-${String(index).padStart(12, "0")}`,
  );
  return {
    invoke: (request) => {
      const item = request.items[0] as { readonly limit?: number } | undefined;
      const limit = item?.limit ?? PAGE_MAXIMUM;
      if (limit < 1 || limit > PAGE_MAXIMUM) {
        return Promise.resolve({ status: "refused", code: "invalid_input", detail: null });
      }
      return Promise.resolve(page(ids.slice(0, limit), size > limit ? "more" : null));
    },
  };
}

/** One table that renders the widget query's definition and loads it through the stub. */
function SeamTable(props: { size: number }): JSX.Element {
  const definition = WIDGET_QUERY_TABLE;
  const transport = widgetStub(props.size);
  const [state, setState] = createSignal<LoadState<WidgetQueryRow>>(emptyLoad(DEFAULT_CAP));

  async function load(cap: number) {
    const next = startLoad(state(), cap);
    setState(next);
    const outcome = await query(transport, [
      { limit: loadLimit(next.cap, definition.pageMaximum) },
    ]);
    setState((current) => finishLoad(current, next.generation, outcome, definition.rowId));
  }
  void load(DEFAULT_CAP);

  return (
    <LoadedTable
      columns={definition.columns}
      rowId={definition.rowId}
      state={state()}
      load={load}
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
function MemoryTable(props: { size: number }): JSX.Element {
  const [state, setState] = createSignal<LoadState<PalletRow>>(emptyLoad(DEFAULT_CAP));

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
  load(DEFAULT_CAP);

  return <LoadedTable columns={PALLET_COLUMNS} rowId="id" state={state()} load={load} />;
}

/** The DataTable over one load state. A refresh loads again at the cap in force. */
function LoadedTable<Row extends object>(props: {
  columns: readonly DataTableColumn<Row>[];
  rowId: keyof Row & string;
  state: LoadState<Row>;
  load: (cap: number) => void;
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
    />
  );
}

export function TableSections(): JSX.Element {
  return (
    <Section title="Data table" name="DataTable, LoadState">
      <div class="flex flex-col gap-6">
        <For each={[3, 100]}>
          {(size) => (
            <State name={`${size} rows from WIDGET_QUERY_TABLE through the load state, over the stub transport`}>
              <SeamTable size={size} />
            </State>
          )}
        </For>
        <For each={[1000, 10000]}>
          {(size) => (
            <State name={`${size} rows generated in memory`}>
              <MemoryTable size={size} />
            </State>
          )}
        </For>
      </div>
    </Section>
  );
}
