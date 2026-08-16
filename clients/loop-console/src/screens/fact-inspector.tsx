import type { JSX } from "@solidjs/web";
import { Match, Show, Switch, createMemo } from "solid-js";

import type { CapturedValue, ExecutionFact, JsonValue, Run } from "../reader/types";
import { CopyId } from "../ui/copy-id";
import { Disclosure } from "../ui/disclosure";
import { JsonView } from "../ui/json-view";
import { KeyValue } from "../ui/key-value";
import { EmptyState } from "../ui/states";
import { nodeRunTone } from "../ui/status";
import { StatusBadge } from "../ui/status-badge";
import "./fact-inspector.css";

/**
 * §6 step 4's three-lane row disclosure: what this visit of a node was handed
 * and produced (DATA), what it did to the run's context (CONTEXT), and what the
 * platform recorded about the visit itself (STATE).
 *
 * Every absence is spelled out rather than blanked. A blank output slot reads
 * as "this node produced nothing", which is a claim exactly one of the five
 * captured states supports — and on an effect-uncertain run it is the wrong one.
 */

/** §1's grouped digits (`3,412`), pinned to one locale so a render never depends on the machine's. */
const digits = new Intl.NumberFormat("en-US");

export function FactInspector(props: {
  fact: ExecutionFact;
  run: Run;
  /** §6 step 4's trace mode: two lines of JSON, a `[json]` pop, `after unchanged`. */
  compressed?: boolean;
}): JSX.Element {
  return (
    <div class="fact-inspector">
      <DataLane fact={props.fact} compressed={props.compressed ?? false} />
      <ContextLane fact={props.fact} compressed={props.compressed ?? false} />
      <StateLane fact={props.fact} run={props.run} />
    </div>
  );
}

/** §1.2: the lane labels frame the evidence, so they wear the frame face and never carry data. */
function Lane(props: { label: string; children: JSX.Element }): JSX.Element {
  return (
    <div class="fact-lane">
      <p class="fact-lane-label label">{props.label}</p>
      <div class="fact-lane-body">{props.children}</div>
    </div>
  );
}

// ── data ───────────────────────────────────────────────────────────────────

/** §6 step 4's `input | output→port`. */
function DataLane(props: { fact: ExecutionFact; compressed: boolean }): JSX.Element {
  return (
    <Lane label="data">
      <KeyValue label="input">
        <Captured
          value={props.fact.input}
          index={props.fact.index}
          side="input"
          compressed={props.compressed}
        />
      </KeyValue>
      <KeyValue label="output">
        {/* The branch taken. Null is a node that emitted no port at all, never `main`. */}
        <Show
          when={props.fact.outputPort === null ? null : { port: props.fact.outputPort }}
          fallback={<span class="fact-note frame">no port emitted</span>}
        >
          {(held) => <span>→ {held().port}</span>}
        </Show>
        <Captured
          value={props.fact.output}
          index={props.fact.index}
          side="output"
          compressed={props.compressed}
        />
      </KeyValue>
    </Lane>
  );
}

/**
 * One captured value, in whichever of its five states arrived. The four that are
 * not `present` are not interchangeable, and the last two least of all: `errored`
 * says the node failed, `outstanding` says nothing was recorded either way, and
 * rendering the second as the first asserts the conclusion §2.2 exists to stop.
 *
 * The side is carried rather than assumed: one absence names the slot it stands
 * in, and a sentence about the output printed on the input row would be the one
 * thing this lane exists not to do.
 */
function Captured(props: {
  value: CapturedValue;
  index: number;
  side: "input" | "output";
  compressed: boolean;
}): JSX.Element {
  const subject = () => `fact ${props.index} ${props.side}`;

  return (
    <Switch>
      <Match when={props.value.state === "present" ? props.value : null}>
        {(present) => (
          <Json value={present().value} subject={subject()} compressed={props.compressed} />
        )}
      </Match>
      <Match when={props.value.state === "present" ? null : props.value}>
        {(absent) => <EmptyState region={subject()} reason={absence(absent(), props.side)} />}
      </Match>
    </Switch>
  );
}

/** Why there is no value here — one sentence per state, never another state's. */
function absence(
  value: Exclude<CapturedValue, { state: "present" }>,
  side: "input" | "output",
): string {
  switch (value.state) {
    case "capture-off":
      // §2.2's words exactly
      return "capture was off for this run";
    case "too-large":
      // The size and the hash are what the row kept; neither stands in for a value.
      return value.hash === null
        ? `too large to capture — ${digits.format(value.size)} bytes, no hash recorded`
        : `too large to capture — ${digits.format(value.size)} bytes, hash ${value.hash}`;
    case "errored":
      return `the node failed, so there is no ${side}`;
    case "outstanding":
      return "the outcome is unknown — nothing was recorded either way";
  }
}

/**
 * The document itself, or — in trace mode — its first two lines with the whole
 * value one `[json]` pop away. The pop names its subject, so the several views
 * one screen carries never collide.
 */
function Json(props: { value: JsonValue; subject: string; compressed: boolean }): JSX.Element {
  const preview = createMemo(() => twoLines(props.value));
  return (
    <Show when={props.compressed} fallback={<JsonView value={props.value} subject={props.subject} />}>
      {/*
       * A div, not a `pre`: the UA sheet gives `pre` its own monospace face on
       * the element, which beats the §1.2 data face inherited from `body`. The
       * CSS keeps the newlines instead, the way `.source-line` does.
       */}
      <div class="fact-json-preview">{preview().text}</div>
      {/* No pop when those two lines are already the whole document. */}
      <Show when={preview().elided}>
        <Disclosure label={`[json] ${props.subject}`}>
          <JsonView value={props.value} subject={props.subject} />
        </Disclosure>
      </Show>
    </Show>
  );
}

function twoLines(value: JsonValue): { readonly text: string; readonly elided: boolean } {
  const lines = JSON.stringify(value, null, 2).split("\n");
  return lines.length > 2
    ? { text: `${lines.slice(0, 2).join("\n")} …`, elided: true }
    : { text: lines.join("\n"), elided: false };
}

// ── context ────────────────────────────────────────────────────────────────

/**
 * §6 step 4's `before | write | after`, in whichever of the union's three modes
 * this run captured. The mode is a property of the run, so one fact never mixes
 * them — and `absent` is the truthful mode today, since no per-node context
 * exists durably.
 */
/**
 * Whether `after` really is the document `before` was. Checked rather than
 * inferred from `write: null`: trace mode is the one place a wrong claim is not
 * visibly checkable, so a read that contradicts itself prints the document that
 * disproves it instead of the sentence.
 */
function unchanged(before: JsonValue, after: JsonValue): boolean {
  return JSON.stringify(before) === JSON.stringify(after);
}

function ContextLane(props: { fact: ExecutionFact; compressed: boolean }): JSX.Element {
  const subject = () => `fact ${props.fact.index} context`;

  return (
    <Lane label="context">
      <Switch>
        <Match
          when={props.fact.context.mode === "before-write-after" ? props.fact.context : null}
        >
          {(lane) => (
            <>
              <KeyValue label="before">
                <Json
                  value={lane().before}
                  subject={`${subject()} before`}
                  compressed={props.compressed}
                />
              </KeyValue>
              <KeyValue label="write">
                {/*
                 * Held in a wrapper rather than tested for truthiness: `null` is
                 * the one value that means the node left context alone, and a
                 * document that is itself falsy is still a write.
                 */}
                <Show
                  when={lane().write === null ? null : { document: lane().write }}
                  fallback={
                    <span class="fact-note frame">this node left the context unchanged</span>
                  }
                >
                  {(written) => (
                    <>
                      <span class="fact-note frame">
                        replaces the whole document — a write is never a merge
                      </span>
                      <Json
                        value={written().document}
                        subject={`${subject()} write`}
                        compressed={props.compressed}
                      />
                    </>
                  )}
                </Show>
              </KeyValue>
              <KeyValue label="after">
                {/* Trace mode says so rather than reprinting the document `before` already showed. */}
                <Show
                  when={props.compressed && unchanged(lane().before, lane().after)}
                  fallback={
                    <Json
                      value={lane().after}
                      subject={`${subject()} after`}
                      compressed={props.compressed}
                    />
                  }
                >
                  <span class="fact-note frame">after unchanged</span>
                </Show>
              </KeyValue>
            </>
          )}
        </Match>
        <Match when={props.fact.context.mode === "final-only" ? props.fact.context : null}>
          {(lane) => (
            <>
              <p class="fact-note frame">
                one cell for the whole run: the run's final context, which says nothing about what
                this node did
              </p>
              <KeyValue label="final">
                <Json
                  value={lane().final}
                  subject={`${subject()} final`}
                  compressed={props.compressed}
                />
              </KeyValue>
            </>
          )}
        </Match>
        <Match when={props.fact.context.mode === "absent"}>
          {/* Not "the context was empty": nothing was captured, so there is nothing to be empty. */}
          <EmptyState region={subject()} reason="this run captured no context" />
        </Match>
      </Switch>
    </Lane>
  );
}

// ── state ──────────────────────────────────────────────────────────────────

function StateLane(props: { fact: ExecutionFact; run: Run }): JSX.Element {
  /**
   * How many facts this run returned for this node — its visit count. Two facts
   * for one node id are two visits, never a retry: a retry never produces a
   * second fact, because the primary key collapses it, so retries live on
   * `attempt` alone. `returned` is said out loud because a truncated read
   * returns fewer visits than the run made.
   */
  const visits = createMemo(
    () => props.run.facts.filter((other) => other.node === props.fact.node).length,
  );

  return (
    <Lane label="state">
      <KeyValue label="status">
        <StatusBadge status={props.fact.status} tone={nodeRunTone(props.fact.status)} />
      </KeyValue>
      <Show when={props.fact.failure}>
        {(failure) => (
          <KeyValue label="failure">
            {failure().kind}
            {/* Null is the absence the row records; an empty detail is still a detail. */}
            <Show when={failure().detail !== null}> · {failure().detail}</Show>
          </KeyValue>
        )}
      </Show>
      <KeyValue label="attempt">
        {/* Wrapped: attempt 0 is the first execution, not an absent attempt. */}
        <Show
          when={props.fact.attempt === null ? null : { attempt: props.fact.attempt }}
          fallback={<NotRecorded />}
        >
          {(held) => (
            <>
              {held().attempt}{" "}
              {/* §2.2's `retryable ×4`: attempts of one visit, counted from the 0th. */}
              <span class="fact-note frame">×{held().attempt + 1} of this visit</span>
            </>
          )}
        </Show>
      </KeyValue>
      <KeyValue label="occurrence">
        {props.fact.occurrence}{" "}
        <span class="fact-note frame">
          visit {props.fact.occurrence + 1} of {visits()} returned for this node
        </span>
      </KeyValue>
      <KeyValue label="frame">
        {props.fact.frameId}{" "}
        <span class="fact-note frame">
          {/* Null exactly at the root frame, so the absence is the fact. */}
          <Show
            when={props.fact.parentFrameId === null ? null : { parent: props.fact.parentFrameId }}
            fallback={<>root</>}
          >
            {(held) => <>parent {held().parent}</>}
          </Show>
        </span>
      </KeyValue>
      <KeyValue label="duration">
        <Show
          when={props.fact.durationMs === null ? null : { ms: props.fact.durationMs }}
          fallback={<NotRecorded />}
        >
          {(held) => <>{digits.format(held().ms)} ms</>}
        </Show>
      </KeyValue>
      <KeyValue label="plan hash">
        {/* Named by its fact: one run's facts share a hash, and two copy buttons may not share a name. */}
        <CopyId id={props.fact.planHash} label={`fact ${props.fact.index} plan hash`} />
      </KeyValue>
      <KeyValue label="effects">
        {/*
         * §6 step 4 asks for an effect-attempt drill-down on effectful facts and
         * `— pure node` otherwise. The read carries neither: no effect attempts,
         * and no marker saying whether a node is effectful. `nodeType` is an open
         * authored string, so inferring it from there would be a guess this
         * screen may not make. STEP-9.
         */}
        <span class="fact-note frame">
          not recorded — this read carries no effect attempts, and nothing marks a node effectful
        </span>
      </KeyValue>
    </Lane>
  );
}

/** The one spelling of a field the platform did not record. */
function NotRecorded(): JSX.Element {
  return <span class="fact-note frame">not recorded</span>;
}
