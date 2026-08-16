import type { NodeRunStatus, RunStatus } from "../reader/types";

/**
 * The five §1.1 status roles.
 *
 * Colour here is semantic, never decorative, and never the only carrier: a tone
 * travels with its status word, always. Primitives set `data-tone` and paint
 * with `var(--tone)` (see `tone.css`) so no component names a status colour.
 */
export type Tone = "ok" | "fail" | "warn" | "uncertain" | "neutral";

/**
 * Unknown-safe on purpose, and the reason these take `string`: the wire types
 * both statuses as bare strings (the STEP-9 RISK on `ExecutionFact`), so a word
 * this console has never heard of must render as itself, uncoloured, rather
 * than vanish or be claimed as a state it is not.
 */
const runTones: ReadonlyMap<string, Tone> = new Map(
  Object.entries({
    queued: "neutral",
    dispatched: "neutral",
    running: "neutral",
    succeeded: "ok",
    failed: "fail",
    "effect-uncertain": "uncertain",
  } satisfies Record<RunStatus, Tone>),
);

export function runTone(status: string): Tone {
  return runTones.get(status) ?? "neutral";
}

/**
 * §2.0's four tree verdicts: `✓` ok, `✗` fail, `◌` effect-uncertain, `•` none.
 *
 * A separate vocabulary from `Tone` even though it maps onto one, because these
 * are the states an *entity* can be in rather than the five roles a colour can
 * play — and because `none` is a real verdict state (a draft never has one, and
 * a report that has not finalized does not have one yet), not a missing tone.
 */
export type VerdictState = "ok" | "fail" | "uncertain" | "none";

const verdictTones: ReadonlyMap<string, Tone> = new Map(
  Object.entries({
    ok: "ok",
    fail: "fail",
    uncertain: "uncertain",
    none: "neutral",
  } satisfies Record<VerdictState, Tone>),
);

export function verdictTone(state: VerdictState): Tone {
  return verdictTones.get(state) ?? "neutral";
}

const nodeRunTones: ReadonlyMap<string, Tone> = new Map(
  Object.entries({
    started: "neutral",
    success: "ok",
    error: "fail",
  } satisfies Record<NodeRunStatus, Tone>),
);

export function nodeRunTone(status: string): Tone {
  return nodeRunTones.get(status) ?? "neutral";
}
