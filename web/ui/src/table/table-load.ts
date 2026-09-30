/**
 * The read of one QueryTable over its table definition.
 *
 * The request of one load holds the fixed and chosen scope, the server search,
 * the limit and the sort. A query streams its load when the transport can: it
 * opens the stream with the cap and the sort, and hands its rows over in
 * batches. A bounded list is one read, with the limit and the sort. The caller
 * owns the scope and the sort, which a load reads when it starts. Each load
 * runs through the load state of `@wamn/web-runtime`. A new load aborts the
 * stream of the last one, and a batch or an outcome of an older load is
 * dropped.
 *
 * The table loads again after every write on the transport that it did not
 * send itself. While an inline edit is open, the load is held: a new load
 * waits until the edit is saved or dropped, and then the last one asked for
 * runs.
 *
 * Whether the set is complete is the answer of the last load that ended. A
 * load in progress keeps that answer and, over a complete set, its rows, so a
 * set table stays as it is until the load ends.
 */

import { type Accessor, createMemo, createSignal, onCleanup } from "solid-js";

import {
  afterWrites,
  appendRows,
  boundedPage,
  callOperation,
  emptyLoad,
  endLoad,
  type FieldMap,
  finishLoad,
  fromWire,
  isStreamReply,
  type JsonValue,
  keepLoad,
  type LoadEnd,
  type LoadPage,
  type LoadState,
  loadLimit,
  openStream,
  type Outcome,
  readLoadLines,
  readMember,
  replaceRow as replaceLoadedRow,
  startLoad,
  type Transport,
  writeMember,
} from "@wamn/web-runtime";

import { announceOutcome } from "../outcome";
import type { TableSort } from "./columns";
import type { QueryTableDefinition, QueryTableFilter } from "./query-definition";
import type { QueryViewState } from "./query-view";
import type { ScopeFilter } from "./scope-bar";

/** The row cap of a table before the operator changes it. */
export const DEFAULT_CAP = 1000;

/** The sort one read sends: the wire name of one field and its direction. */
interface LoadSort {
  readonly field: string;
  readonly direction: TableSort["direction"];
}

/**
 * One bound of a range, as the contract type spells it. A time control gives
 * a local time, which the request sends as an instant. An int32 and a float64
 * are JSON numbers, and a numeric is a decimal string.
 */
function rangeBound(type: string | undefined, text: string): JsonValue {
  switch (type) {
    case "timestamptz":
      return new Date(text).toISOString();
    case "int32":
    case "float64":
      return Number(text);
    default:
      return text;
  }
}

/** The request value of one scope filter: a list, one value, a range, or whether the field is empty. */
function scopeValue(filter: QueryTableFilter, chosen: ScopeFilter): JsonValue {
  if (chosen.empty !== undefined) {
    return chosen.empty;
  }
  if (chosen.range !== undefined) {
    const { min, max } = chosen.range;
    return {
      ...(min === "" ? {} : { min: rangeBound(filter.type, min) }),
      ...(max === "" ? {} : { max: rangeBound(filter.type, max) }),
    };
  }
  return filter.list ? [...chosen.values] : chosen.values[0]!;
}

/** The field map of the values inside one member of a result, or none. */
function nestedFields(fields: FieldMap, member: string): FieldMap {
  const entry = fields[member];
  return entry === undefined || typeof entry === "string" ? {} : entry.fields;
}

/** True when the caller fixes a filter, so the operator does not choose it. */
export const fixedBy = (fixed: object | undefined, filter: QueryTableFilter) =>
  readMember(fixed ?? {}, filter.input) !== undefined;

export interface QueryLoadOptions<TResult> {
  readonly transport: Transport;
  /** What an operator calls the screen, which its outcomes name. */
  readonly label: string;
  /** Input the parent fixes, which the operator does not edit. */
  readonly fixed?: object | undefined;
  /** Called with every outcome of the read. */
  readonly onOutcome?: ((outcome: Outcome<TResult>) => void) | undefined;
  /** What the read is asked: the scope, the server search, the sort and the cap. */
  readonly query: () => QueryViewState;
}

export interface QueryLoad<TRow extends object> {
  /** The load state that the table renders. */
  readonly state: Accessor<LoadState<TRow>>;
  /** True when the last load that ended read every row. */
  readonly complete: Accessor<boolean>;
  /**
   * True when the last load that ended stopped before the last row. It is
   * false before the first load ends, so the table shows no hint of a partial
   * set while it cannot know yet (wamn-28n8).
   */
  readonly partial: Accessor<boolean>;
  /** The rows of the last complete set while a load runs over it, or else the loaded rows. */
  readonly rows: Accessor<readonly TRow[]>;
  /** Starts a new load at the cap the query holds. */
  readonly reload: () => void;
  /** Holds every new load while `held` is true, and runs the last one asked for when it ends. */
  readonly hold: (held: boolean) => void;
  /** Puts one row, as a write left it, in place of the loaded row with its id. */
  readonly replaceRow: (row: TRow) => void;
  /** Sends one write of this table, whose result the table puts in place itself, so it starts no load. */
  readonly write: <T>(send: () => Promise<T>) => Promise<T>;
}

export function createQueryLoad<TRow extends object, TResult>(
  definition: QueryTableDefinition<TRow>,
  options: QueryLoadOptions<TResult>,
): QueryLoad<TRow> {
  const transport = options.transport;
  const [state, setState] = createSignal<LoadState<TRow>>(emptyLoad(DEFAULT_CAP));
  let held = false;
  // The cap of the last load asked for while held, or undefined.
  let waiting: number | undefined;
  // The stream of the load in flight, which a new load aborts.
  let streaming: AbortController | undefined;
  // True while this table sends a write, whose result it puts in place itself.
  let writing = false;
  // The request and the ETag of the last load that read to its end. The
  // same request revalidates its rows with that ETag.
  let lastLoad: { readonly request: string; readonly etag: string } | undefined;

  const report = (outcome: Outcome<unknown>) => {
    options.onOutcome?.(outcome as Outcome<TResult>);
    if (outcome.status !== "completed") {
      announceOutcome(outcome, options.label);
    }
  };

  // A band the operator did not set is left out, so the server reads its default days.
  const request = (limit: number, sort: LoadSort | undefined): object => {
    const query = options.query();
    let request = { ...options.fixed } as object;
    for (const chosen of query.scope) {
      const filter = definition.filters.find((declared) => declared.field === chosen.field);
      if (filter !== undefined && !fixedBy(options.fixed, filter)) {
        request = writeMember(request, filter.input, scopeValue(filter, chosen));
      }
    }
    if (definition.search !== undefined && query.find !== "") {
      request = writeMember(request, definition.search.input, query.find);
    }
    if (definition.limitInput !== null) {
      request = writeMember(request, definition.limitInput, limit);
    }
    if (sort !== undefined && definition.sortFieldInput !== null) {
      request = writeMember(request, definition.sortFieldInput, sort.field);
    }
    if (sort !== undefined && definition.sortDirectionInput !== null) {
      request = writeMember(request, definition.sortDirectionInput, sort.direction);
    }
    return request;
  };

  /** Reads one stream of `cap` rows, and hands its rows to `onRows` in batches. */
  async function stream(
    cap: number,
    sort: LoadSort | undefined,
    signal: AbortSignal,
    onRows: (rows: readonly TRow[]) => void,
  ): Promise<LoadEnd | { readonly status: "unchanged" }> {
    const item = request(cap, sort);
    const key = JSON.stringify(item);
    const opened = await openStream<LoadPage<TRow>>(
      transport,
      definition.read,
      item,
      signal,
      lastLoad?.request === key ? lastLoad.etag : undefined,
    );
    if (isStreamReply(opened) && "notModified" in opened) {
      return { status: "unchanged" };
    }
    let end: LoadEnd;
    if (isStreamReply(opened)) {
      const rowFields = nestedFields(definition.read.result, "item");
      end = await readLoadLines(
        opened.body,
        (row) => fromWire(row, rowFields) as TRow,
        onRows,
        definition.read.route.contract.errors,
      );
      lastLoad = end.status === "completed" && opened.etag !== null ? { request: key, etag: opened.etag } : undefined;
    } else if (opened.status === "completed") {
      onRows(opened.value.item);
      end = { status: "completed", value: { more: opened.value.nextCursor !== null } };
    } else {
      end = opened as Outcome<never>;
    }
    // A streamed load reports only a load that did not complete. A load that
    // a newer load cancelled did not fail, so it reports nothing (wamn-v43a).
    if (end.status !== "completed" && !signal.aborted) {
      report(end as Outcome<unknown>);
    }
    return end;
  }

  // A page read streams its load, up to the cap, when the transport can.
  const streams = definition.rows === "item" && transport.openStream !== undefined;

  const load = async (cap: number) => {
    if (held) {
      waiting = cap;
      return;
    }
    const last = state();
    const next = startLoad(last, cap);
    // A read sorts by one field, so the load sends the first field of the sort, when the read declares it.
    const [first] = options.query().sort;
    const declared = definition.sortFields.find((field) => field.field === first?.field);
    const sort =
      first === undefined || declared === undefined ? undefined : { field: declared.wire, direction: first.direction };
    setState(next);
    if (streams) {
      streaming?.abort();
      const controller = new AbortController();
      streaming = controller;
      const end = await stream(next.cap, sort, controller.signal, (rows) => {
        // The table renders each batch at once, so the update's time is its
        // render time: the User Timing entry `wamn:load-render` (wamn-utci.6).
        const start = performance.now();
        setState((current) => appendRows(current, next.generation, rows));
        performance.measure("wamn:load-render", { start, detail: { rows: rows.length } });
      });
      setState((current) =>
        end.status === "unchanged"
          ? keepLoad(current, next.generation, last.fullyRead)
          : endLoad(current, next.generation, end, definition.rowId),
      );
      return;
    }
    const limit = definition.pageMaximum === null ? next.cap : loadLimit(next.cap, definition.pageMaximum);
    const outcome = await callOperation<LoadPage<TRow> & { readonly rows: readonly TRow[] }>(
      transport,
      definition.read,
      [request(limit, sort)],
    );
    report(outcome);
    const page = definition.rows === "rows" ? boundedPage(outcome) : outcome;
    setState((current) => finishLoad(current, next.generation, page, definition.rowId));
  };

  const reload = () => void load(options.query().cap);

  const hold = (next: boolean) => {
    held = next;
    const cap = waiting;
    if (!held && cap !== undefined) {
      waiting = undefined;
      void load(cap);
    }
  };

  const write = async <T>(send: () => Promise<T>) => {
    writing = true;
    try {
      return await send();
    } finally {
      writing = false;
    }
  };

  onCleanup(
    // eslint-disable-next-line solid/reactivity -- the listener runs on a write, an event, not in a tracked scope.
    afterWrites(transport, () => {
      if (!writing) {
        reload();
      }
    }),
  );

  const complete = createMemo<boolean>((last) => (state().busy ? last : state().fullyRead), false);
  const partial = createMemo<boolean>(
    (last) => (state().busy ? last : state().endedAt !== null && !state().fullyRead && state().refusal === null),
    false,
  );
  const rows = createMemo<readonly TRow[]>((last) => (state().busy && complete() ? last : state().rows), []);

  return {
    state,
    complete,
    partial,
    rows,
    reload,
    hold,
    replaceRow: (row) => setState((current) => replaceLoadedRow(current, row, definition.rowId)),
    write,
  };
}
