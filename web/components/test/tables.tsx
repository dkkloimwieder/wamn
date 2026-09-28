/**
 * The two tables the table tests render.
 *
 * `queryTable` renders a QueryTable over a memory read of the given rows, and
 * waits for its first load to end. Each request the read took is in `asked`.
 * `setTable` renders a SetTable over the given rows, with its state in
 * signals the test can read. `withUpdate` gives a definition an update, and
 * `updating` a transport that answers it.
 */

import { render, waitFor } from "@solidjs/testing-library";
import { createSignal } from "solid-js";

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
import type { JsonValue, Outcome, Transport } from "@wamn/web-runtime";

import { type MemoryRequest, memoryDefinition, memoryRoute, memoryTransport } from "../gallery/memory.js";

/** The text of the table's status: when the load started and ended, and what it could not read. */
export const statusText = () => document.querySelector('[data-slot="table-status"]')?.textContent ?? "";

/** Waits until no load runs. */
export const settled = () =>
  waitFor(() => {
    if (!/ended/.test(statusText()) || /Loading/.test(statusText())) {
      throw new Error("a load is running");
    }
  });

export interface QueryTableOptions<Row extends object> {
  readonly name?: string;
  readonly columns: readonly TableColumn<Row>[];
  readonly rows: () => readonly Row[];
  /** True while the read has more rows than it returned. */
  readonly more?: () => boolean;
  /** The read never answers. */
  readonly never?: boolean;
  /** The read refuses with this code. */
  readonly refuse?: string;
  readonly scopeFilters?: readonly (keyof Row & string)[];
  readonly sortFields?: readonly (keyof Row & string)[];
  readonly sortMaxFields?: number;
  readonly urlKey?: string;
  readonly hiddenFields?: readonly (keyof Row & string)[];
  readonly groupedFields?: readonly (keyof Row & string)[];
  readonly timeZone?: string;
  /** Adds to the definition, such as its update or its actions. */
  readonly definition?: (definition: QueryTableDefinition<Row>) => QueryTableDefinition<Row>;
  /** Wraps the memory transport, such as with the writes it answers. */
  readonly transport?: (memory: Transport) => Transport;
  readonly onOpen?: { readonly [operation: string]: (row: Row) => void };
}

export async function queryTable<Row extends object>(options: QueryTableOptions<Row>) {
  const asked: MemoryRequest[] = [];
  const memory = memoryTransport(options.rows, {
    asked,
    ...(options.more === undefined ? {} : { more: options.more }),
    ...(options.never === undefined ? {} : { never: options.never }),
    ...(options.refuse === undefined ? {} : { refuse: options.refuse }),
  });
  const base = memoryDefinition(options.name ?? "rows", options.columns, {
    ...(options.scopeFilters === undefined ? {} : { scopeFilters: options.scopeFilters }),
    ...(options.sortFields === undefined ? {} : { sortFields: options.sortFields }),
    ...(options.sortMaxFields === undefined ? {} : { sortMaxFields: options.sortMaxFields }),
  });
  render(() => (
    <div style={{ height: "800px" }}>
      <QueryTable
        definition={options.definition?.(base) ?? base}
        transport={options.transport?.(memory) ?? memory}
        label={options.name ?? "rows"}
        urlKey={options.urlKey}
        hiddenFields={options.hiddenFields}
        groupedFields={options.groupedFields}
        timeZone={options.timeZone}
        onOpen={options.onOpen}
      />
    </div>
  ));
  if (options.never !== true) {
    await settled();
  }
  return { asked };
}

/** One update a table sent: the row it names, the revision it expects, and the change. */
export interface UpdateItem {
  readonly id: string;
  readonly expected: unknown;
  readonly change: { readonly [field: string]: unknown };
}

const UPDATE = memoryRoute("rows", "update");

/** The definition with an update of `fields`, which sends the row's `rowVersion` as its revision. */
export const withUpdate =
  <Row extends object>(fields: readonly string[]) =>
  (definition: QueryTableDefinition<Row>): QueryTableDefinition<Row> => ({
    ...definition,
    update: {
      binding: { route: UPDATE, request: {}, result: {} },
      keyInput: ["id"],
      revisionInput: ["expected"],
      revisionField: "rowVersion",
      supplied: [],
      fields: fields.map((field) => ({ field, input: ["change", field] })),
    },
  });

/**
 * A transport over the memory read that also answers the update with
 * `answer`, and keeps each update in `updates`. `writes` holds the listeners
 * of the writes a transport reports, which a test calls as another session's
 * write.
 */
export function updating(answer: (item: UpdateItem) => Outcome<JsonValue>) {
  const updates: UpdateItem[] = [];
  const writes = new Set<() => void>();
  const transport = (memory: Transport): Transport => ({
    invoke: async (request) => {
      if (request.operation !== UPDATE.operation) {
        return memory.invoke(request);
      }
      const item = request.items[0] as unknown as UpdateItem;
      updates.push(item);
      return answer(item);
    },
    onWrite: (listener) => {
      writes.add(listener);
      return () => writes.delete(listener);
    },
  });
  return { transport, updates, writes };
}

export interface SetTableOptions<Row extends object> {
  readonly columns: readonly TableColumn<Row>[];
  readonly rows: readonly Row[];
  readonly sortMaxFields?: number | undefined;
  readonly hiddenFields?: readonly (keyof Row & string)[] | undefined;
  readonly groupedFields?: readonly (keyof Row & string)[] | undefined;
  readonly timeZone?: string | undefined;
  readonly weekStart?: number | undefined;
}

export function setTable<Row extends { readonly id: string }>(options: SetTableOptions<Row>) {
  const time = (field: string) => options.columns.find((column) => column.field === field)?.type === "timestamptz";
  const [view, setView] = createSignal(
    defaultSetView((options.groupedFields ?? []).map((field) => ({ field, time: time(field) }))),
  );
  const [grid, setGrid] = createSignal(
    defaultGridView(
      options.columns.map((column) => column.field),
      options.hiddenFields,
    ),
  );
  const [sort, setSort] = createSignal<readonly TableSort[]>([]);
  render(() => (
    <div style={{ height: "800px" }} class="flex flex-col">
      <SetTable
        name="rows"
        rows={options.rows}
        rowId={(row) => row.id}
        columns={builtColumns(options.columns)}
        view={view()}
        onView={setView}
        grid={grid()}
        onGrid={setGrid}
        sort={sort()}
        onSort={setSort}
        sortMaxFields={options.sortMaxFields ?? 1}
        timeZone={options.timeZone}
        weekStart={options.weekStart}
      />
    </div>
  ));
  return { view, grid, sort };
}
