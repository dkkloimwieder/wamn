import type { JSX } from "@solidjs/web";
import { Show } from "solid-js";

import type { RunFailure } from "../reader/types";
import { Disclosure } from "./disclosure";
import { JsonView } from "./json-view";
import { KeyValue } from "./key-value";
import "./failure-panel.css";

/**
 * §2.2's RUN FAILURE section: kind · at · detail · `▸ raw`.
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
      <KeyValue label="at">{props.failure.node ?? <NotRecorded />}</KeyValue>
      <KeyValue label="detail">{props.failure.detail ?? <NotRecorded />}</KeyValue>
      {/*
       * A null raw is no disclosure at all, not an empty one: offering `▸ raw`
       * over nothing promises evidence the read never returned.
       */}
      <Show when={props.failure.raw !== null}>
        <Disclosure label="raw failure">
          <JsonView value={props.failure.raw} subject="the raw failure" />
        </Disclosure>
      </Show>
    </div>
  );
}

/** The one spelling of absence here: the frame face says it, the data face never lies about it. */
function NotRecorded(): JSX.Element {
  return <span class="failure-absent frame">not recorded</span>;
}
