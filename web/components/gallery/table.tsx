/**
 * The DataTable over rows generated in memory, at 100, 1000 and 10,000 rows.
 *
 * Each state owns its load, as the load-state source will: a new cap starts a
 * new load, which regenerates the rows and sets the times. A set larger than
 * the cap loads the cap and is not fully read.
 */

import { createSignal, For, type JSX } from "solid-js";

import { DataTable, type DataTableColumn } from "@wamn/ui";

import { Section, State } from "./section.js";

/** One generated pallet row. */
interface PalletRow {
  readonly id: string;
  readonly code: string;
  readonly quantity: number;
  readonly weight: string;
  readonly createdAt: string;
}

const COLUMNS: readonly DataTableColumn<PalletRow>[] = [
  { field: "code", label: "code", type: "text" },
  { field: "quantity", label: "quantity", type: "int32" },
  { field: "weight", label: "weight", type: "numeric" },
  { field: "createdAt", label: "created at", type: "timestamptz" },
  { field: "id", label: "id", type: "uuid" },
];

/** The time a generated load takes, so the loading state shows. */
const LOAD_MS = 300;

/** The default cap of the platform table. */
const DEFAULT_CAP = 1000;

function pallets(count: number): PalletRow[] {
  return Array.from({ length: count }, (_, index) => ({
    id: `00000000-0000-4000-8000-${String(index).padStart(12, "0")}`,
    code: `P-${String(index + 1).padStart(5, "0")}`,
    quantity: 1 + (index % 48),
    weight: (100 + (index % 900) / 4).toFixed(2),
    createdAt: `2026-09-${String(1 + (index % 28)).padStart(2, "0")}T12:00:00.000000Z`,
  }));
}

/** One table over a set of `size` rows, loaded up to its cap. */
function GeneratedTable(props: { size: number }): JSX.Element {
  const [cap, setCap] = createSignal(DEFAULT_CAP);
  const [rows, setRows] = createSignal<readonly PalletRow[]>([]);
  const [busy, setBusy] = createSignal(false);
  const [startedAt, setStartedAt] = createSignal<Date | null>(null);
  const [endedAt, setEndedAt] = createSignal<Date | null>(null);

  function load(next: number) {
    setCap(next);
    setBusy(true);
    setStartedAt(new Date());
    setEndedAt(null);
    setTimeout(() => {
      setRows(pallets(Math.min(props.size, next)));
      setEndedAt(new Date());
      setBusy(false);
    }, LOAD_MS);
  }
  load(DEFAULT_CAP);

  return (
    <DataTable
      columns={COLUMNS}
      rowId="id"
      rows={rows()}
      fullyRead={props.size <= cap()}
      busy={busy()}
      cap={cap()}
      onCapChange={load}
      startedAt={startedAt()}
      endedAt={endedAt()}
    />
  );
}

export function TableSections(): JSX.Element {
  return (
    <Section title="Data table" name="DataTable">
      <div class="flex flex-col gap-6">
        <For each={[100, 1000, 10000]}>
          {(size) => (
            <State name={`${size} rows`}>
              <GeneratedTable size={size} />
            </State>
          )}
        </For>
      </div>
    </Section>
  );
}
