/**
 * A read over rows held in memory, for the gallery and the component tests.
 *
 * `memoryDefinition` writes a table definition in the shape the emitter
 * writes one: a paged read with a limit, a list scope filter on each named
 * field, and a sort on each named field. `memoryTransport` answers that read
 * from the rows: it keeps the rows each scope filter names, sorts them by the
 * request's sort, and returns up to the limit. It says more rows exist when
 * the rows go past the limit, or when `more` says so, so a test can hold a set
 * that is not fully read.
 */

import type { QueryTableDefinition, TableColumn } from "@wamn/ui";
import type { JsonValue, OperationRoute, Transport } from "@wamn/web-runtime";

/** The route of a memory operation, which only a stub answers. */
export const memoryRoute = (name: string, verb = "query"): OperationRoute => ({
  operation: `gallery:${name}/${verb}@1.0.0`,
  method: verb === "query" ? "GET" : "POST",
  template: `/${name}/${verb}`,
  freshOnly: false,
  contract: {
    resultClass: "page",
    partialSchema: null,
    errors: [],
    replay: null,
    direct: true,
    type: verb === "query" ? "query" : "command",
    transaction: "implicit",
    // A memory operation reads or writes the one relation its name gives.
    reads: verb === "query" ? [`gallery.${name}`] : [],
    writes: verb === "query" ? null : [`gallery.${name}`],
  },
});

/** A table definition over a memory read of `columns`, keyed by `id`. */
export function memoryDefinition<TRow extends object>(
  name: string,
  columns: readonly TableColumn<TRow>[],
  options: {
    /** The fields a scope filter can name. */
    readonly scopeFilters?: readonly (keyof TRow & string)[];
    /** The fields the read can sort by. */
    readonly sortFields?: readonly (keyof TRow & string)[];
    readonly sortMaxFields?: number;
  } = {},
): QueryTableDefinition<TRow> {
  return {
    name,
    read: { route: memoryRoute(name), request: {}, result: { next_cursor: "nextCursor" } },
    rows: "item",
    rowId: ["id" as keyof TRow & string],
    pageMaximum: null,
    limitInput: ["limit"],
    sortFieldInput: ["sort", "field"],
    sortDirectionInput: ["sort", "direction"],
    filters: (options.scopeFilters ?? []).map((field) => ({ field, input: ["filter", field], list: true })),
    scopeFilters: options.scopeFilters ?? [],
    sortFields: (options.sortFields ?? []).map((field) => ({ field, wire: field })),
    sortMaxFields: options.sortMaxFields ?? 1,
    columns: columns.map(({ cell: _cell, ...column }) => column),
    actions: [],
    childTables: [],
  };
}

/** One request the memory read took. */
export interface MemoryRequest {
  readonly limit?: number;
  readonly filter?: { readonly [field: string]: readonly string[] };
  readonly sort?: { readonly field: string; readonly direction: string };
}

/**
 * A transport that answers a memory read from `rows`. Each request lands in
 * `asked`. A read that never answers leaves its load in progress, and a
 * refusal code refuses every read.
 */
export function memoryTransport<TRow extends object>(
  rows: () => readonly TRow[],
  options: {
    readonly more?: () => boolean;
    readonly asked?: MemoryRequest[];
    readonly never?: boolean;
    readonly refuse?: string;
  } = {},
): Transport {
  return {
    invoke: (request) => {
      const item = (request.items[0] ?? {}) as MemoryRequest;
      options.asked?.push(item);
      if (options.never === true) {
        return new Promise(() => {});
      }
      if (options.refuse !== undefined) {
        return Promise.resolve({ status: "refused", code: options.refuse, detail: null });
      }
      const kept = rows().filter((row) =>
        Object.entries(item.filter ?? {}).every(([field, values]) =>
          values.includes(String((row as { readonly [name: string]: unknown })[field])),
        ),
      );
      const sort = item.sort;
      const sign = sort?.direction === "descending" ? -1 : 1;
      const value = (row: TRow) => String((row as { readonly [name: string]: unknown })[sort?.field ?? ""]);
      const ordered = sort === undefined ? kept : [...kept].sort((a, b) => sign * value(a).localeCompare(value(b)));
      const limit = item.limit ?? ordered.length;
      return Promise.resolve({
        status: "completed",
        value: {
          item: ordered.slice(0, limit) as unknown as JsonValue,
          next_cursor: ordered.length > limit || options.more?.() === true ? "more" : null,
        },
      });
    },
  };
}
