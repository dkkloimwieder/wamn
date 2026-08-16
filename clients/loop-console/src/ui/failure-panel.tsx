import type { JSX } from "@solidjs/web";
import { Show } from "solid-js";

import type { RunFailure } from "../reader/types";
import { Disclosure } from "./disclosure";
import { JsonView } from "./json-view";
import { KeyValue } from "./key-value";
import "./failure-panel.css";

/**
 * §2.2's RUN FAILURE section: kind · at · detail, with `▸ raw` on the detail's
 * own line — one deviation from the wireframe, on the `at` row, recorded there.
 *
 * Only `kind` is always there. The failing node and the reason are `runs`
 * columns the wire `RunFailure` does not carry, so each absence is spelled out
 * rather than left blank: a blank beside `at` reads as a run that failed
 * nowhere, which is a claim this panel is not entitled to make.
 */
export function FailurePanel(props: { failure: RunFailure }): JSX.Element {
  return (
    <div class="failure-panel">
      <KeyValue label="kind">{props.failure.kind}</KeyValue>
      {/*
       * §2.2 prints this row as `at fetch-inventory (http-request)`. This one
       * prints the node alone, and that is a decision rather than an omission.
       *
       * `RunFailure` is the `runs` row's failure columns and carries no node
       * type. The type exists only on `ExecutionFact.nodeType`, which the reader
       * has already joined from the flow definition by node id (STEP-9 RISK), so
       * the parenthetical would have to be joined a second time here — from the
       * returned facts, by matching `fail_node`. That second join is not total:
       * the facts are a bounded window (`FactCount.truncated`, §2.2's `showing
       * 200 of 3,412`) and `fail_node` need not name a node this read returned a
       * fact for at all, so a miss cannot be told apart from a node that has no
       * type. Printing nothing on a miss is the blank this panel's header
       * forbids, and printing `not recorded` would deny a type the platform does
       * record. Nor would the join be free: this panel is handed one failure, and
       * taking the fact list beside it would make a panel over `runs` columns
       * depend on `node_runs` rows for a parenthetical. The type is not lost in
       * the meantime — §2.2's execution table prints node · type one section
       * below, on the failing node's own row.
       *
       * What would change it: the reader resolves the failing node's type where
       * it already resolves `nodeType` for the facts, and hands it over on
       * `RunFailure`. This panel then keeps taking exactly one value and prints
       * the parenthetical when the read supplies it, with nothing to join here.
       */}
      <KeyValue label="at">{recorded(props.failure.node)}</KeyValue>
      {/*
       * §2.2 trails the detail row with `▸ raw` on that same line, so the toggle
       * is a sibling of the pair rather than part of its value: the raw failure
       * is its own evidence, not a continuation of the reason, and the `dl` must
       * keep `detail` associated with the detail alone.
       */}
      <div class="failure-detail-row">
        <KeyValue label="detail">{recorded(props.failure.detail)}</KeyValue>
        {/*
         * A null raw is no disclosure at all, not an empty one: offering `▸ raw`
         * over nothing promises evidence the read never returned. It turns on
         * `raw` alone — an unrecorded reason says nothing about whether the
         * platform returned the raw failure beside it, so the toggle still
         * trails a `not recorded` detail.
         */}
        <Show when={props.failure.raw !== null}>
          <Disclosure label="raw failure">
            <JsonView value={props.failure.raw} subject="the raw failure" />
          </Disclosure>
        </Show>
      </div>
    </div>
  );
}

/**
 * A nullable `runs` column as the reader gets it, or the absence spelled out.
 *
 * `??` alone would only catch the null. `fail_node` and `fail_reason` are plain
 * nullable text and nothing between the database and here rejects `""` — no
 * reader seam normalizes them yet (STEP-9 RISK on `RunFailure`) — so an empty
 * or whitespace-only column rendered as itself paints exactly the blank cell
 * this panel's header forbids. Blank is treated as no record: it is the smaller
 * claim of the two, since a blank beside `at` reads as a run that failed
 * nowhere. What is recorded is printed unaltered, whitespace and all.
 */
function recorded(value: string | null): JSX.Element {
  return value !== null && value.trim() !== "" ? value : <NotRecorded />;
}

/** The one spelling of absence here: the frame face says it, the data face never lies about it. */
function NotRecorded(): JSX.Element {
  return <span class="failure-absent frame">not recorded</span>;
}
