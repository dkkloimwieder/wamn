import type { JSX } from "@solidjs/web";

import type { Run } from "../reader/types";
import { toHash } from "../routing/route";
import { CopyId } from "../ui/copy-id";
import { runTone } from "../ui/status";
import { VerdictBar } from "../ui/verdict-bar";
import "./run-verdict.css";

/**
 * §2.2's verdict line — the `RunStatus` word, then the run-level fail kind and
 * the failing node when the run failed: `FAILED · retry-exhausted at
 * fetch-inventory`. Exported and pure so every fixture's line is assertable
 * without a render, and so step 5's report verdict can reuse the shape.
 *
 * Both tails are conditional because both go missing truthfully. A run that did
 * not fail carries no failure at all, and `RunFailure.node` is a `runs` column
 * the wire does not carry (STEP-9 RISK), so a failed run frequently has no node
 * to name — which must read as the status and kind alone, never `at null`.
 *
 * The kind is dropped only when it would repeat the status word itself, on an
 * effect-uncertain run. A terminalized run is the case that makes the kind
 * load-bearing rather than decorative: its status is `failed` while its kind is
 * still `effect-uncertain`, so the line keeps the kind and cannot be read as an
 * ordinary failure.
 */
export function runVerdictLine(run: Run): string {
  const status = run.status.toUpperCase();
  const failure = run.failure;
  if (failure === null) {
    return status;
  }
  const kind = failure.kind === run.status ? "" : ` · ${failure.kind}`;
  const at = failure.node === null ? "" : ` at ${failure.node}`;
  return `${status}${kind}${at}`;
}

/** §2.2's verdict bar for a run, and the identity line beneath it. */
export function RunVerdict(props: { run: Run }): JSX.Element {
  return (
    <VerdictBar
      tone={runTone(props.run.status)}
      verdict={runVerdictLine(props.run)}
      identity={
        <span class="run-identity">
          run <CopyId id={props.run.runId} label="run" /> ·{" "}
          <a
            class="run-identity-draft"
            // the draft route addresses the draft by id and carries its revision
            // as an opaque segment; the line reads the flow, which is what an
            // author recognizes
            href={toHash({
              kind: "draft",
              id: props.run.draft.draftId,
              revision: String(props.run.draft.revision),
            })}
          >
            draft {props.run.draft.flowId}@{props.run.draft.revision}{" "}
            <span aria-hidden="true">→</span>
          </a>{" "}
          · trace <CopyId id={props.run.traceId} label="trace" /> · captured {props.run.capture}
        </span>
      }
    />
  );
}
