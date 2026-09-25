/**
 * The load state of one table: the rows of its last load, whether that load
 * read the whole set, and when it ran.
 *
 * Every function returns a new value, so a component stores the state wherever
 * its framework keeps state, as with `page.ts`. The caller does the one read
 * and hands its outcome back with the generation that `startLoad` gave it.
 *
 * Until streamed loads exist, a load is one page read with a limit of the cap
 * or the page maximum, whichever is lower. No cursor after it means the set is
 * fully read. A cursor means rows exist beyond the cap, and no loop follows it.
 */

import { refusalSentence } from "./refusal.js";
import type { Outcome } from "./wire.js";

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
}

/** The page a load reads: its rows, and the cursor of rows beyond them. */
export interface LoadPage<Row> {
  readonly item: readonly Row[];
  readonly nextCursor: string | null;
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
  };
}

/** The limit of the one page read: the cap, or the page maximum if lower. */
export function loadLimit(cap: number, pageMaximum: number): number {
  return Math.min(cap, pageMaximum);
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
  rowId: keyof Row & string,
  now: Date = new Date(),
): LoadState<Row> {
  if (generation !== state.generation) {
    return state;
  }
  const failed = (refusal: string): LoadState<Row> => ({
    ...state,
    rows: [],
    fullyRead: false,
    busy: false,
    refusal,
    endedAt: now,
  });
  if (outcome.status !== "completed") {
    return failed(
      outcome.status === "refused"
        ? refusalSentence(outcome.code)
        : outcome.status === "uncertain"
          ? outcome.reason
          : outcome.status,
    );
  }
  const seen = new Set<string>();
  for (const row of outcome.value.item) {
    const id = String(row[rowId]);
    if (seen.has(id)) {
      return failed(`The load returned the row id ${id} twice.`);
    }
    seen.add(id);
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
