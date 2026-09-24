/**
 * The state of one read screen: the rows it holds, the cursor it can follow,
 * and whether a request is in flight.
 *
 * Every function returns a new value, so a component stores the state wherever
 * its framework keeps state. Nothing here knows a framework.
 *
 * The rules come from the screen plan and from the terminal. A page appends,
 * which `show_result` in `crates/client/tui/src/screen.rs` does. A read clears
 * its rows when an input changes, so the next reply starts a new list.
 */

import type { Outcome } from "./wire.js";

/** What one read screen holds between replies. */
export interface PageState<Row> {
  /** Every row the screen shows, oldest page first. */
  readonly rows: readonly Row[];
  /** The cursor of the next page, or null when no page follows. */
  readonly cursor: string | null;
  /** Whether a request is in flight. */
  readonly busy: boolean;
  /**
   * Why the last read did not complete, or null. A screen shows this in place
   * of its empty message, because an empty list is a completed read.
   */
  readonly refusal: string | null;
}

/**
 * The state of a screen that has read nothing.
 *
 * An input change clears the rows this way. The cursor goes with them, because
 * a cursor names a position in the list that the old input produced.
 */
export function emptyPage<Row>(): PageState<Row> {
  return { rows: [], cursor: null, busy: false, refusal: null };
}

/** Mark one request as in flight. */
export function startRead<Row>(state: PageState<Row>): PageState<Row> {
  return { ...state, busy: true, refusal: null };
}

/**
 * Take the first page of a read, or the whole of a bounded list.
 *
 * The rows replace what the screen held, because this is a new list.
 */
export function firstPage<Row>(
  rows: readonly Row[],
  cursor: string | null = null,
): PageState<Row> {
  return { rows: [...rows], cursor, busy: false, refusal: null };
}

/** Take the next page of a read, which follows the rows already shown. */
export function appendPage<Row>(
  state: PageState<Row>,
  rows: readonly Row[],
  cursor: string | null,
): PageState<Row> {
  return { rows: [...state.rows, ...rows], cursor, busy: false, refusal: null };
}

/** Mark a request that produced no rows as finished. */
export function stopRead<Row>(state: PageState<Row>): PageState<Row> {
  return { ...state, busy: false };
}

/**
 * Mark a read that did not complete as finished, with why.
 *
 * The rows the screen held stay. A refusal states its code, and an uncertain
 * reply states its reason, so the screen never reads as an empty list.
 */
export function failedRead<Row>(state: PageState<Row>, outcome: Outcome<unknown>): PageState<Row> {
  const refusal =
    outcome.status === "refused"
      ? (outcome.code ?? "refused")
      : outcome.status === "uncertain"
        ? outcome.reason
        : outcome.status;
  return { ...state, busy: false, refusal };
}

/** Whether the screen can ask for another page. */
export function hasNextPage<Row>(state: PageState<Row>): boolean {
  return state.cursor !== null && !state.busy;
}
