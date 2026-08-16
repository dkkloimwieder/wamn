import type { JSX } from "@solidjs/web";
import { Show } from "solid-js";

import { wireFailureKind, wireRunStatus } from "../reader/run-vocabulary";
import type {
  Assertion,
  AssertionFailure,
  FlowFailureKind,
  RunTerminalStatus,
} from "../reader/types";
import { JsonView } from "../ui/json-view";
import { KeyValue } from "../ui/key-value";
import { nodeRunTone, runTone, type Tone } from "../ui/status";
import { StatusBadge } from "../ui/status-badge";
import "./report-assertion.css";

/**
 * §2.3's `expected / observed` pair, one per failed assertion, in the data face.
 *
 * The expected side is the assertion the author wrote, rendered by family; the
 * observed side is free text and only free text — the evaluator emits one
 * English sentence built with Rust debug formatting and there is no structured
 * observed value anywhere — so it is printed exactly as it arrived. Parsing it
 * into a pair to match the expected side's shape would be this screen inventing
 * the structure the platform does not have.
 *
 * **The vocabulary rule (dkk, wamn-dggp.6).** An assertion's expected side is
 * written in the DURABLE run vocabulary, and `run →` lands on a run screen that
 * speaks the WIRE one. The durable word is the value here, always; the wire word
 * is a parenthetical gloss, and only where the two differ — so
 * `completed (the run reads succeeded)` prepares the author for the word they
 * are about to see, while `failed` and `effect-uncertain`, which are the same
 * word on both sides, take no gloss at all. `infrastructure-failure` is the
 * absence: authors assert it and the wire vocabulary has no word for it, so it
 * prints itself and says so. Never the wire word as the value, and never a
 * nearest neighbour for the word that has none.
 */

/**
 * Tone travels with the WIRE word (`run-vocabulary.ts`): `runTone("completed")`
 * is `neutral` by design, so toning the durable word directly would paint a
 * passing run grey. A word with no wire form has no tone to take — the run
 * screen never paints it — and stays neutral rather than borrowing one.
 */
function runStatusTone(status: RunTerminalStatus): Tone {
  const wire = wireRunStatus(status);
  return wire.form === "wire" ? runTone(wire.status) : "neutral";
}

function runStatusGloss(status: RunTerminalStatus): string | null {
  const wire = wireRunStatus(status);
  switch (wire.form) {
    case "wire":
      // Same word on both sides: a gloss here would restate the value and teach
      // the author a distinction that is not there.
      return wire.status === status ? null : `the run reads ${wire.status}`;
    case "no-wire-form":
      return "no wire form — the run screen has no word for this status";
    case "unknown-word":
      // Reachable only by a cast today, and still not a licence to guess.
      return "no wire form this console knows";
  }
}

function flowFailureGloss(kind: FlowFailureKind): string | null {
  const wire = wireFailureKind(kind);
  switch (wire.form) {
    case "wire":
      return wire.kind === kind ? null : `the run reads ${wire.kind}`;
    case "no-wire-form":
      // The three budget kinds: assertable and unreadable in the same breath.
      return "no wire form — the run screen has no word for this kind";
    case "unknown-word":
      return "no wire form this console knows";
  }
}

/** The wire word, where there is one to say, or the named absence of one. */
function Gloss(props: { gloss: string | null }): JSX.Element {
  return (
    <Show when={props.gloss}>
      {(gloss) => <span class="report-gloss frame"> ({gloss()})</span>}
    </Show>
  );
}

/**
 * The expected side, by family. Each renders what its own family carries, and
 * nothing else.
 *
 * A `switch` rather than §2.3's `Switch`/`Match`, for the reason `runStatusGloss`
 * and `flowFailureGloss` above are switches: the `default` clause anchors the
 * arms to the union. `AssertionFailureView` prints the family name
 * unconditionally over this, so a family with no arm here would draw its own
 * name above an empty line — a blank where the assertion should be
 * (wamn-dggp.29).
 */
function expectedLine(assertion: Assertion, subject: string): JSX.Element {
  switch (assertion.family) {
    case "run-terminal-outcome":
      return (
        <span class="report-expected-line">
          <StatusBadge status={assertion.status} tone={runStatusTone(assertion.status)} />
          <Gloss gloss={runStatusGloss(assertion.status)} />
        </span>
      );
    case "typed-flow-failure":
      return (
        // No badge: a failure kind is not a status, and the five §1.1 tones are
        // the status roles. Giving it one would invent a semantics for it.
        <span class="report-expected-line">
          {assertion.kind}
          <Gloss gloss={flowFailureGloss(assertion.kind)} />
        </span>
      );
    case "named-node-terminal":
      return (
        <span class="report-expected-line">
          {/* §2.3's `check-stock → error`, with the flow the node belongs to:
              the assertion names a flow/node pair and the node id alone does
              not identify one. */}
          {assertion.flowId}/{assertion.nodeId} <span aria-hidden="true">→</span>{" "}
          <StatusBadge status={assertion.status} tone={nodeRunTone(assertion.status)} />
          {/* The node vocabulary is one word on both sides — `node_runs.status`
              and the assertion's `NodeTerminalStatus` are the same set — so
              nothing is glossed here. */}
          <Show when={assertion.status === "error"}>
            <Show
              when={assertion.failureKind}
              fallback={
                // Required when the status is `error`, so a missing one is a
                // field the read did not carry, not a kind that is absent.
                <span class="report-absent frame"> · failure kind not recorded</span>
              }
            >
              {(kind) => <> · {kind()}</>}
            </Show>
          </Show>
        </span>
      );
    case "terminal-respond":
      return (
        <>
          <span class="report-expected-line">status {assertion.status}</span>
          {/* The subject names which evidence the copy button acts on: one
              screen carries a view per terminal-respond assertion. */}
          <JsonView value={assertion.body} subject={`${subject} expected body`} />
        </>
      );
    default:
      // The anchor, in `run-vocabulary.ts`'s spelling: `assertion` is `never`
      // while the arms above cover the union, so a fifth family fails to compile
      // on this line rather than reaching the render. What it draws is for the
      // cast that gets here anyway — the absence named, since a family printed
      // over an empty line would claim the assertion carried nothing.
      assertion satisfies never;
      return (
        <span class="report-absent frame">this console has no rendering for this family</span>
      );
  }
}

/**
 * The expected side, in a reactive position: the fragment is what re-runs the
 * family switch when the assertion changes, since a component body runs once.
 */
function Expected(props: { assertion: Assertion; subject: string }): JSX.Element {
  return <>{expectedLine(props.assertion, props.subject)}</>;
}

export function AssertionFailureView(props: {
  failure: AssertionFailure;
  /** Which assertion of which case this is, so the views on one screen never collide. */
  subject: string;
}): JSX.Element {
  return (
    <div class="report-assertion">
      <KeyValue label="expected">
        <div class="report-expected">
          <span class="report-family">{props.failure.expected.family}</span>
          <Expected assertion={props.failure.expected} subject={props.subject} />
        </div>
      </KeyValue>
      <KeyValue label="observed">
        {/* Verbatim: one sentence of free text, never reflowed into a pair. */}
        <span class="report-observed">{props.failure.observed}</span>
      </KeyValue>
    </div>
  );
}
