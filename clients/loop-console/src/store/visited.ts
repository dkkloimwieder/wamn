import { createSignal } from "solid-js";

import { proxyTarget } from "../config";
import type { Draft, Report, Run } from "../reader/types";
import type { Route } from "../routing/route";
import type { VerdictState } from "../ui/status";

/**
 * The visited store: what this browser has looked at, which is the console's
 * only substitute for the list endpoints it does not have (§2.0).
 *
 * Ids and cached display text, nothing else. The store never holds a `Run`, a
 * `Report` or a `Draft` — an entity that has moved on since the visit would
 * otherwise be rendered from a stale copy of itself, and the tree would state
 * verdicts the platform no longer agrees with. What it caches is exactly what
 * §2.0 draws without re-reading: the glyph, the label, and when it was seen.
 *
 * Written by the entity screens on every settled read (§6's "4–5 write it"),
 * read by the side panel and the command palette ("6 and 7 read it").
 */

export type VisitedKind = "run" | "report" | "draft";

export type VisitedEntry = {
  readonly kind: VisitedKind;
  readonly id: string;
  /** Drafts only — the route addresses a draft by id *and* revision. */
  readonly revision: number | null;
  /**
   * §2.0's tree text. A draft is `orders@17`: `flowId` is what an author
   * recognizes and it is not in the route, so it cannot be re-derived from the
   * id later. A run and a report are their own id, which is all they have.
   */
  readonly label: string;
  readonly verdict: VerdictState;
  /** Epoch ms of the last visit, so §2.0's relative time is computed, never cached stale. */
  readonly visitedAt: number;
};

/** §2.0's "capped (~20 per kind)", applied per kind so a busy run day cannot evict every draft. */
export const VISITED_CAP_PER_KIND = 20;

/**
 * §6 step 6 namespaces the store by (target, project, environment, kind). The
 * console knows only the target today — project and environment arrive with
 * step 9's real proxy config — so the key carries the target and the kinds are
 * partitioned inside the value. Widening the key later abandons the old one,
 * which is correct rather than lossy: entries visited against another project
 * were never entries of this one.
 */
export const visitedStorageKey = `wamn-loop-console/visited/${proxyTarget}`;

/** Identity of an entry: a draft is identified by its revision as well as its id. */
function sameEntity(a: VisitedEntry, b: VisitedEntry): boolean {
  return a.kind === b.kind && a.id === b.id && a.revision === b.revision;
}

const kinds: ReadonlySet<string> = new Set<VisitedKind>(["run", "report", "draft"]);
const verdicts: ReadonlySet<string> = new Set<VerdictState>(["ok", "fail", "uncertain", "none"]);

/**
 * Storage is a foreign input: it survives upgrades, it is editable by hand, and
 * a shape this build does not recognize must cost the reader their history at
 * worst — never a screen that will not mount. Every entry is checked, and one
 * bad entry is dropped rather than condemning the rest.
 */
function decodeEntry(value: unknown): VisitedEntry | null {
  if (typeof value !== "object" || value === null) {
    return null;
  }
  const entry = value as Record<string, unknown>;
  const { kind, id, revision, label, verdict, visitedAt } = entry;
  if (typeof kind !== "string" || !kinds.has(kind)) {
    return null;
  }
  if (typeof id !== "string" || id.length === 0) {
    return null;
  }
  if (typeof label !== "string" || label.length === 0) {
    return null;
  }
  if (typeof verdict !== "string" || !verdicts.has(verdict)) {
    return null;
  }
  if (typeof visitedAt !== "number" || !Number.isFinite(visitedAt)) {
    return null;
  }
  // A draft without a revision addresses nothing, and a non-draft with one
  // addresses something that does not exist — both are unusable as routes.
  const hasRevision = typeof revision === "number" && Number.isSafeInteger(revision);
  if (kind === "draft" ? !hasRevision : revision !== null) {
    return null;
  }
  return {
    kind: kind as VisitedKind,
    id,
    revision: hasRevision ? (revision as number) : null,
    label,
    verdict: verdict as VerdictState,
    visitedAt,
  };
}

export function decodeVisited(raw: string | null): readonly VisitedEntry[] {
  if (raw === null) {
    return [];
  }
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    // Not JSON at all — someone else's key, or a half-written value.
    return [];
  }
  if (!Array.isArray(parsed)) {
    return [];
  }
  const entries: VisitedEntry[] = [];
  for (const value of parsed) {
    const entry = decodeEntry(value);
    if (entry !== null) {
      entries.push(entry);
    }
  }
  return entries;
}

export function encodeVisited(entries: readonly VisitedEntry[]): string {
  return JSON.stringify(entries);
}

/**
 * Storage is optional, and every one of its calls can throw — disabled cookies,
 * private mode, a full quota. The console is view-only and the store is a
 * convenience, so a browser that refuses it gets a session-lived store rather
 * than an error: losing the history is not worth failing a read the author came
 * for.
 */
function loadStored(): readonly VisitedEntry[] {
  try {
    return decodeVisited(window.localStorage.getItem(visitedStorageKey));
  } catch {
    return [];
  }
}

function persist(entries: readonly VisitedEntry[]): void {
  try {
    window.localStorage.setItem(visitedStorageKey, encodeVisited(entries));
  } catch {
    // The in-memory store still stands; only its survival across a reload is lost.
  }
}

/**
 * The store's value is this array; the signal beside it only announces that it
 * moved.
 *
 * Solid 2 stages a write until the graph flushes, so a signal read taken
 * straight after a write still answers with the old value. A store that derived
 * its next state from that read would drop a visit whenever two screens
 * recorded in one tick — the second would compute from a history that did not
 * yet contain the first. Keeping the array authoritative removes the hazard
 * rather than asking every caller to flush at the right moment.
 *
 * `ownedWrite` because every writer is a screen recording its own visit from
 * inside its own owned scope, which is exactly what the option licenses.
 */
let current: readonly VisitedEntry[] = loadStored();
const [changed, setChanged] = createSignal(0, { ownedWrite: true });
let revision = 0;

function commit(next: readonly VisitedEntry[]): void {
  current = next;
  revision += 1;
  setChanged(revision);
}

/**
 * Most recent first, every kind together — §2.0 groups them, the palette ranks
 * them. Reading `changed` is what subscribes the caller; the value returned is
 * the authoritative one, so a reader is never handed a history it has already
 * been told is out of date.
 */
export function visitedEntries(): readonly VisitedEntry[] {
  changed();
  return current;
}

export function visitedOfKind(kind: VisitedKind): readonly VisitedEntry[] {
  return visitedEntries().filter((entry) => entry.kind === kind);
}

/**
 * Record a visit, most-recent-first, one entry per entity.
 *
 * A re-visit *replaces* rather than adds: the display text is refreshed (§2.0's
 * "refreshed whenever the entity is visited") and the entity moves to the
 * front, so the store stays a history of entities and never of visits.
 */
export function recordVisit(entry: VisitedEntry): void {
  const others = current.filter((existing) => !sameEntity(existing, entry));
  const merged = [entry, ...others];
  // Capped per kind, in place, so trimming runs does not reorder drafts.
  const kept: VisitedEntry[] = [];
  const counts = new Map<VisitedKind, number>();
  for (const candidate of merged) {
    const seen = counts.get(candidate.kind) ?? 0;
    if (seen < VISITED_CAP_PER_KIND) {
      counts.set(candidate.kind, seen + 1);
      kept.push(candidate);
    }
  }
  commit(kept);
  persist(kept);
}

/** §2.0's `clear visited`, which §6 step 7 moves into the palette. */
export function clearVisited(): void {
  commit([]);
  try {
    window.localStorage.removeItem(visitedStorageKey);
  } catch {
    // As above: the visible store is already empty, which is what was asked for.
  }
}

/** The route an entry addresses — the one place a draft's revision becomes a segment. */
export function routeOfVisit(entry: VisitedEntry): Route {
  if (entry.kind === "draft") {
    return { kind: "draft", id: entry.id, revision: String(entry.revision ?? 0) };
  }
  return { kind: entry.kind, id: entry.id };
}

/**
 * §2.0's run glyph. `queued`, `dispatched` and `running` have no verdict yet —
 * `none` rather than a guess, because a run in flight has not failed.
 */
export function runVerdictState(run: Run): VerdictState {
  switch (run.status) {
    case "succeeded":
      return "ok";
    case "failed":
      /*
       * A terminalized run reads back as `failed` with an `effect-uncertain`
       * kind. §2.0's `◌` is the state an author must not misread, so the tree
       * keeps showing it after the operator has acted: the run is resolved, not
       * ordinary.
       */
      return run.failure?.kind === "effect-uncertain" ? "uncertain" : "fail";
    case "effect-uncertain":
      return "uncertain";
    case "queued":
    case "dispatched":
    case "running":
      return "none";
  }
}

export function visitFromRun(run: Run, at: number): VisitedEntry {
  return {
    kind: "run",
    id: run.runId,
    revision: null,
    label: run.runId,
    verdict: runVerdictState(run),
    visitedAt: at,
  };
}

/** A report that has not finalized has no verdict yet, which is `none` and not a failure. */
export function reportVerdictState(report: Report): VerdictState {
  if (report.state === "pending") {
    return "none";
  }
  return report.passed === report.total ? "ok" : "fail";
}

export function visitFromReport(report: Report, at: number): VisitedEntry {
  return {
    kind: "report",
    id: report.reportId,
    revision: null,
    label: report.reportId,
    verdict: reportVerdictState(report),
    visitedAt: at,
  };
}

/**
 * A draft has no verdict at all — §2.0's `•`. Whether it parses is not one: a
 * half-finished draft is a normal object here (§2.4), and marking it `fail`
 * would tell an author the platform rejected something it stored exactly as
 * asked.
 */
export function visitFromDraft(draft: Draft, at: number): VisitedEntry {
  return {
    kind: "draft",
    id: draft.draftId,
    revision: draft.revision,
    label: `${draft.flowId}@${draft.revision}`,
    verdict: "none",
    visitedAt: at,
  };
}
