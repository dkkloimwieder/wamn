import type { FlowFailureKind, RunFailureKind, RunStatus, RunTerminalStatus } from "./types";

/**
 * The durable↔wire run vocabulary, mapped explicitly.
 *
 * One state, two words: an assertion is written and evaluated in the durable
 * words (§2.3's `expected run-terminal-outcome: completed`) and a run arrives in
 * the wire words (§2.2's `SUCCEEDED`), and the report's `run →` handoff crosses
 * between them in one click. The reader keeps the wire words because that is
 * what it will receive; screens that need the other side ask here rather than
 * assuming the two words are the same word (wamn-dggp.14, dkk 2026-08-15).
 *
 * Every function is total and exhaustive over its union — a vocabulary that
 * gains or loses a word fails to compile here rather than falling through to a
 * wrong one — and where a word has no counterpart the answer is named, never
 * undefined and never the nearest neighbour. The tables are maps for the reason
 * `runTone` uses one (`src/ui/status.ts`): a word that arrives through a cast or
 * a future projection decoder answers `unknown-word` rather than a prototype
 * member or a state it is not.
 *
 * Tone travels with the WIRE word, always. `runTone("completed")` is `neutral`
 * on purpose, so a screen that prints a durable word must colour it by passing
 * `wireRunStatus(…).status` to `runTone` — toning the durable word directly
 * paints a passing run grey.
 */

/** The answer for a word neither vocabulary knows; reachable only by cast today. */
const unknownWord = { form: "unknown-word" } as const;

/** The wire word for a durable terminal status, or the named absence of one. */
export type WireRunStatus =
  | { readonly form: "wire"; readonly status: RunStatus }
  | { readonly form: "no-wire-form" }
  | { readonly form: "unknown-word" };

/**
 * `infrastructure-failure` is the absence: authors assert it, and the wire
 * vocabulary has no word for it at all. Answering `failed` would tell an author
 * their infrastructure failure was an ordinary one, so this answers nothing.
 */
const wireRunStatuses: ReadonlyMap<string, WireRunStatus> = new Map(
  Object.entries({
    completed: { form: "wire", status: "succeeded" },
    failed: { form: "wire", status: "failed" },
    "infrastructure-failure": { form: "no-wire-form" },
    "effect-uncertain": { form: "wire", status: "effect-uncertain" },
  } satisfies Record<RunTerminalStatus, WireRunStatus>),
);

export function wireRunStatus(status: RunTerminalStatus): WireRunStatus {
  return wireRunStatuses.get(status) ?? unknownWord;
}

/** The durable terminal word a wire status stands for, or the named absence. */
export type DurableRunStatus =
  | { readonly form: "durable"; readonly status: RunTerminalStatus }
  | { readonly form: "not-terminal" }
  | { readonly form: "unknown-word" };

/**
 * `queued`, `dispatched` and `running` answer `not-terminal` rather than "no
 * durable form": `dispatched` and `running` are durable words and `queued` is
 * wire-invented, but none of the three is a terminal outcome an assertion can
 * name, which is the only question this direction asks.
 *
 * `failed` inverts to `failed` because `infrastructure-failure` has no wire
 * form to be confused with. STEP-9 RISK — how the projection reports an
 * infrastructure-failed run is unspecified; if it turns out to collapse one to
 * `failed`, this inverse stops being a single answer and needs a third named
 * variant carrying both candidates, never a widened `durable` one.
 */
const durableRunStatuses: ReadonlyMap<string, DurableRunStatus> = new Map(
  Object.entries({
    queued: { form: "not-terminal" },
    dispatched: { form: "not-terminal" },
    running: { form: "not-terminal" },
    succeeded: { form: "durable", status: "completed" },
    failed: { form: "durable", status: "failed" },
    "effect-uncertain": { form: "durable", status: "effect-uncertain" },
  } satisfies Record<RunStatus, DurableRunStatus>),
);

export function durableRunStatus(status: RunStatus): DurableRunStatus {
  return durableRunStatuses.get(status) ?? unknownWord;
}

/** The wire word for an assertable flow failure kind, or the named absence. */
export type WireFailureKind =
  | { readonly form: "wire"; readonly kind: RunFailureKind }
  | { readonly form: "no-wire-form" }
  | { readonly form: "unknown-word" };

/**
 * Three of `FlowFailureKind`'s seven have no wire form — the budget kinds
 * `runaway-budget`, `depth-budget` and `dispatch-budget`. A run that spent a
 * budget is assertable and unreadable in the same breath, so the answer is the
 * absence rather than `terminal`, which would name a failure that did not occur.
 */
const wireFailureKinds: ReadonlyMap<string, WireFailureKind> = new Map(
  Object.entries({
    terminal: { form: "wire", kind: "terminal" },
    "retry-exhausted": { form: "wire", kind: "retry-exhausted" },
    "invalid-input": { form: "wire", kind: "invalid-input" },
    "runaway-budget": { form: "no-wire-form" },
    "effect-uncertain": { form: "wire", kind: "effect-uncertain" },
    "depth-budget": { form: "no-wire-form" },
    "dispatch-budget": { form: "no-wire-form" },
  } satisfies Record<FlowFailureKind, WireFailureKind>),
);

export function wireFailureKind(kind: FlowFailureKind): WireFailureKind {
  return wireFailureKinds.get(kind) ?? unknownWord;
}

/** The flow failure kind a wire run failure asserts as, or the named absence. */
export type AssertableFailureKind =
  | { readonly form: "assertable"; readonly kind: FlowFailureKind }
  | { readonly form: "not-assertable" }
  | { readonly form: "unknown-word" };

/**
 * `deadline-exhausted` is the absence in this direction: it is not a durable run
 * failure at all, only a `CaseFailureKind`, so no `typed-flow-failure` assertion
 * can name it and a run that carries it cannot be matched against one.
 */
const assertableFailureKinds: ReadonlyMap<string, AssertableFailureKind> = new Map(
  Object.entries({
    "invalid-input": { form: "assertable", kind: "invalid-input" },
    "retry-exhausted": { form: "assertable", kind: "retry-exhausted" },
    "deadline-exhausted": { form: "not-assertable" },
    "effect-uncertain": { form: "assertable", kind: "effect-uncertain" },
    terminal: { form: "assertable", kind: "terminal" },
  } satisfies Record<RunFailureKind, AssertableFailureKind>),
);

export function assertableFailureKind(kind: RunFailureKind): AssertableFailureKind {
  return assertableFailureKinds.get(kind) ?? unknownWord;
}
