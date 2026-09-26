/**
 * The load state of one table: the rows of its last load, whether that load
 * read the whole set, and when it ran.
 *
 * Every function returns a new value, so a component stores the state wherever
 * its framework keeps state, as with `page.ts`. The caller does the one read
 * and hands its outcome back with the generation that `startLoad` gave it.
 *
 * A load of a query streams its rows (`docs/architecture/execution.md`):
 * `readLoadLines` reads the reply lines and hands the rows over in batches,
 * `appendRows` puts each batch in the state, and `endLoad` ends the load with
 * its outcome line. No rows beyond the cap means the set is fully read.
 *
 * A load of a bounded list is one read, which `finishLoad` takes whole, and it
 * is always fully read.
 */

import { refusalSentence } from "./refusal.js";
import type { ErrorCase, JsonValue, Outcome } from "./wire.js";

/** What one table holds between loads. */
export interface LoadState<Row> {
  /** The rows of the last load, each with its own row id. */
  readonly rows: readonly Row[];
  /** Whether the last load ended with no rows beyond the cap. */
  readonly fullyRead: boolean;
  /** Whether a load is in flight. */
  readonly busy: boolean;
  /** Why the last load did not complete, or null. */
  readonly refusal: string | null;
  /** When the last load started, or null before the first. */
  readonly startedAt: Date | null;
  /** When the last load ended, or null while it runs. */
  readonly endedAt: Date | null;
  /** The row cap in force. */
  readonly cap: number;
  /** The number of the last load. A result of an older one is dropped. */
  readonly generation: number;
  /** The rows of the load in flight that have arrived so far. */
  readonly arrived: number;
}

/** The page a load reads: its rows, and the cursor of rows beyond them. */
export interface LoadPage<Row> {
  readonly item: readonly Row[];
  readonly nextCursor: string | null;
}

/**
 * The outcome of a bounded list as the one page of a load.
 *
 * A bounded list answers with every row and no cursor, so its load is always
 * fully read.
 */
export function boundedPage<Row>(
  outcome: Outcome<{ readonly rows: readonly Row[] }>,
): Outcome<LoadPage<Row>> {
  switch (outcome.status) {
    case "completed":
      return { status: "completed", value: { item: outcome.value.rows, nextCursor: null } };
    case "partiallyCompleted":
      return {
        ...outcome,
        committedResult: { item: outcome.committedResult.rows, nextCursor: null },
      };
    default:
      return outcome;
  }
}

/** The state of a table that has loaded nothing. */
export function emptyLoad<Row>(cap: number): LoadState<Row> {
  return {
    rows: [],
    fullyRead: false,
    busy: false,
    refusal: null,
    startedAt: null,
    endedAt: null,
    cap,
    generation: 0,
    arrived: 0,
  };
}

/**
 * Start a new load, with a new generation.
 *
 * A cap change, a scope change and a refresh each start one. The caller holds
 * the scope. The rows of the last load stay on screen until this one ends.
 */
export function startLoad<Row>(
  state: LoadState<Row>,
  cap: number = state.cap,
  now: Date = new Date(),
): LoadState<Row> {
  return {
    ...state,
    fullyRead: false,
    busy: true,
    refusal: null,
    startedAt: now,
    endedAt: null,
    cap,
    generation: state.generation + 1,
    arrived: 0,
  };
}

/** The limit of the one page read: the cap, or the page maximum if lower. */
export function loadLimit(cap: number, pageMaximum: number): number {
  return Math.min(cap, pageMaximum);
}

/** The fields of a row whose values together name it: one field, or several. */
export type RowKey<Row> = readonly (keyof Row & string)[];

/**
 * The id of one row: the value of its one key field, or the key values as a
 * JSON list when several fields name the row.
 */
export function rowKey<Row>(row: Row, key: RowKey<Row>): string {
  const [only] = key;
  return key.length === 1 && only !== undefined ? String(row[only]) : JSON.stringify(key.map((field) => row[field]));
}

/**
 * Take the outcome of the load numbered `generation`.
 *
 * An outcome of an older load is dropped, and the state is returned as it was.
 * A duplicate row id fails the load, because it means a broken query.
 * A refusal reads as the forms read one, and the load keeps no rows.
 */
export function finishLoad<Row>(
  state: LoadState<Row>,
  generation: number,
  outcome: Outcome<LoadPage<Row>>,
  rowId: RowKey<Row>,
  now: Date = new Date(),
): LoadState<Row> {
  if (generation !== state.generation) {
    return state;
  }
  if (outcome.status !== "completed") {
    return failedLoad(state, loadFailure(outcome), now);
  }
  const duplicate = duplicateRow(outcome.value.item, rowId);
  if (duplicate !== null) {
    return failedLoad(state, duplicate, now);
  }
  return {
    ...state,
    rows: [...outcome.value.item],
    fullyRead: outcome.value.nextCursor === null,
    busy: false,
    refusal: null,
    endedAt: now,
  };
}

/** How a streamed load ended: completed, with whether rows exist past the cap, or not. */
export type LoadEnd = Outcome<{ readonly more: boolean }>;

/**
 * Put one batch of a streamed load into the state.
 *
 * The first batch replaces the rows of the last load, and each later batch
 * follows it, so the table shows rows as they arrive. A batch of an older
 * load is dropped.
 */
export function appendRows<Row>(
  state: LoadState<Row>,
  generation: number,
  rows: readonly Row[],
): LoadState<Row> {
  if (generation !== state.generation || !state.busy) {
    return state;
  }
  return {
    ...state,
    rows: state.arrived === 0 ? [...rows] : [...state.rows, ...rows],
    arrived: state.arrived + rows.length,
  };
}

/**
 * End the streamed load numbered `generation` with its outcome line.
 *
 * An end of an older load is dropped. A duplicate row id fails the load, and
 * a load that did not complete keeps no rows, as `finishLoad` does.
 */
export function endLoad<Row>(
  state: LoadState<Row>,
  generation: number,
  end: LoadEnd,
  rowId: RowKey<Row>,
  now: Date = new Date(),
): LoadState<Row> {
  if (generation !== state.generation) {
    return state;
  }
  if (end.status !== "completed") {
    return failedLoad(state, loadFailure(end), now);
  }
  const rows = state.arrived === 0 ? [] : state.rows;
  const duplicate = duplicateRow(rows, rowId);
  if (duplicate !== null) {
    return failedLoad(state, duplicate, now);
  }
  return {
    ...state,
    rows,
    fullyRead: !end.value.more,
    busy: false,
    refusal: null,
    endedAt: now,
  };
}

/** The state of a load that did not complete: no rows, and why. */
function failedLoad<Row>(state: LoadState<Row>, refusal: string, now: Date): LoadState<Row> {
  return { ...state, rows: [], fullyRead: false, busy: false, refusal, endedAt: now };
}

/** Why a load did not complete, as the forms read a refusal. */
function loadFailure(outcome: Outcome<unknown>): string {
  return outcome.status === "refused"
    ? refusalSentence(outcome.code, outcome.text)
    : outcome.status === "uncertain"
      ? outcome.reason
      : outcome.status;
}

/** The failure of a load that holds one row id twice, which means a broken query. */
function duplicateRow<Row>(rows: readonly Row[], rowId: RowKey<Row>): string | null {
  const seen = new Set<string>();
  for (const row of rows) {
    const id = rowKey(row, rowId);
    if (seen.has(id)) {
      return `The load returned the row id ${id} twice.`;
    }
    seen.add(id);
  }
  return null;
}

/** Schedules one hand-over of rows, and returns the function that cancels it. */
export type BatchTick = (flush: () => void) => () => void;

/**
 * Rows reach the table once per animation frame in a page, and every 50 ms
 * where no frames run. A new rows list rebuilds the table's whole row model,
 * so rows never go over one at a time.
 */
export const batchTick: BatchTick = (flush) => {
  if (typeof requestAnimationFrame === "function") {
    const frame = requestAnimationFrame(() => flush());
    return () => cancelAnimationFrame(frame);
  }
  const timer = setTimeout(flush, 50);
  return () => clearTimeout(timer);
};

/**
 * Read the reply lines of one streamed load, and return how it ended.
 *
 * Each line is one JSON value: `{"row":…}` for each row, then one
 * `{"outcome":…}`. The body is decoded as a stream, because a chunk can end
 * inside a character, and lines are buffered, because a chunk can end inside
 * a line. `revive` turns a row's wire spelling into the table's row, and
 * `onRows` takes the rows in batches. A malformed line fails the load, and a
 * body that ends without its outcome line fails it too: neither is skipped.
 * `errors` names the text of each declared refusal.
 */
export async function readLoadLines<Row>(
  body: ReadableStream<Uint8Array>,
  revive: (row: JsonValue) => Row,
  onRows: (rows: readonly Row[]) => void,
  errors: readonly ErrorCase[] = [],
  tick: BatchTick = batchTick,
): Promise<LoadEnd> {
  // The DOM types declare the decoder's input as BufferSource, which a body's
  // Uint8Array chunks are.
  const decoder = new TextDecoderStream() as unknown as ReadableWritablePair<string, Uint8Array>;
  const reader = body.pipeThrough(decoder).getReader();
  let pending: Row[] = [];
  let cancel: (() => void) | null = null;
  const flush = () => {
    cancel = null;
    if (pending.length > 0) {
      const rows = pending;
      pending = [];
      // Each hand-over leaves the User Timing mark `wamn:load-batch`, so the
      // batch interval reads from the performance timeline.
      performance.mark("wamn:load-batch", { detail: { rows: rows.length } });
      onRows(rows);
    }
  };
  const end = (outcome: LoadEnd): LoadEnd => {
    cancel?.();
    flush();
    reader.cancel().catch(() => undefined);
    return outcome;
  };
  let buffered = "";
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) {
        return end(uncertainLoad("the load ended without its outcome line"));
      }
      buffered += value;
      let start = 0;
      for (
        let newline = buffered.indexOf("\n", start);
        newline !== -1;
        newline = buffered.indexOf("\n", start)
      ) {
        const line = buffered.slice(start, newline);
        start = newline + 1;
        const parsed = parseLine(line);
        if (parsed === null) {
          return end(uncertainLoad("the load sent a malformed line"));
        }
        if ("row" in parsed) {
          pending.push(revive(parsed.row));
          cancel ??= tick(flush);
        } else {
          return end(outcomeLine(parsed.outcome, errors));
        }
      }
      buffered = buffered.slice(start);
    }
  } catch (error) {
    return end(uncertainLoad(`the load did not complete: ${String(error)}`));
  }
}

function uncertainLoad(reason: string): LoadEnd {
  return { status: "uncertain", reason, retryRefusal: null };
}

/** One reply line: a row or the outcome, or null for anything else. */
function parseLine(
  line: string,
): { readonly row: JsonValue } | { readonly outcome: JsonValue } | null {
  let value: unknown;
  try {
    value = JSON.parse(line);
  } catch {
    return null;
  }
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    return null;
  }
  const keys = Object.keys(value);
  if (keys.length !== 1) {
    return null;
  }
  const members = value as { readonly row?: JsonValue; readonly outcome?: JsonValue };
  if (members.row !== undefined && typeof members.row === "object" && members.row !== null) {
    return { row: members.row };
  }
  return members.outcome === undefined ? null : { outcome: members.outcome };
}

/** How the outcome line says the load ended. */
function outcomeLine(outcome: JsonValue, errors: readonly ErrorCase[]): LoadEnd {
  const members =
    outcome !== null && typeof outcome === "object" && !Array.isArray(outcome)
      ? (outcome as { readonly [key: string]: JsonValue })
      : {};
  const value = members["value"];
  const more =
    value !== null && typeof value === "object" && !Array.isArray(value)
      ? (value as { readonly more?: JsonValue }).more
      : undefined;
  if (typeof more === "boolean") {
    return { status: "completed", value: { more } };
  }
  const error = members["error"];
  if (error !== null && typeof error === "object" && !Array.isArray(error)) {
    // The detail is the error without its code, as a page refusal reads.
    const { code: literal, ...detail } = error as { readonly [key: string]: JsonValue };
    const code = typeof literal === "string" ? literal : null;
    const text = errors.find((declared) => declared.literal === code)?.text;
    return {
      status: "refused",
      code,
      detail,
      ...(text === undefined || text === null ? {} : { text }),
    };
  }
  return uncertainLoad(
    "uncertain" in members
      ? "the release cannot say how the load ended"
      : "the load sent a malformed outcome line",
  );
}

/**
 * Put one row in place of the row with the same id, and keep the rest of the
 * load as it was. An inline edit that cannot move the row out of the scope or
 * the order uses it in place of a new load. A row the load does not hold
 * changes nothing.
 */
export function replaceRow<Row>(state: LoadState<Row>, row: Row, rowId: RowKey<Row>): LoadState<Row> {
  const id = rowKey(row, rowId);
  const at = state.rows.findIndex((candidate) => rowKey(candidate, rowId) === id);
  if (at === -1) {
    return state;
  }
  const rows = [...state.rows];
  rows[at] = row;
  return { ...state, rows };
}
