import type { JSX } from "@solidjs/web";

import type { ExecutionFact, FactCount, Run } from "../reader/types";
import { DataTable, type Column } from "../ui/data-table";
import { EmptyState } from "../ui/states";
import { nodeRunTone } from "../ui/status";
import { StatusBadge } from "../ui/status-badge";
import "./execution-table.css";

/**
 * §2.2's execution table: one row per returned fact, in returned order, as a
 * configuration of the step-3 `DataTable`.
 */

/** `main` and `error` are the engine's; a node type only names the others. */
const RESERVED_PORTS: ReadonlySet<string> = new Set(["main", "error"]);

/** Pinned, so the wireframe's `3,412` is the wireframe's and not the runner's locale. */
const thousands = (value: number): string => value.toLocaleString("en-US");

/**
 * §2.2's note column: the single most useful scrap for one row.
 *
 * Precedence, from the wireframe: fact 4 carries a port (`error`) *and* a
 * failure and prints `retryable ×4`; fact 5 carries a repeat occurrence *and* a
 * failure and prints `terminal`. So a row that failed answers "what happened"
 * with why it failed, a row that succeeded answers it with the branch it took,
 * and a row that did neither is distinguished only by which visit it is.
 *
 * `×N` is `attempt + 1` and appears only when the attempt number is both known
 * and past the first: `attempt` is null wherever nothing durable recorded it
 * (the STEP-9 RISK), and neither an unknown attempt nor a first one may print
 * as the `×1` that would claim a retry happened. The count rides the failure
 * kind or not at all — §2.2 never pairs `×N` with anything else, so a node that
 * retried and then succeeded says only which branch it took: the retry is not
 * the most useful scrap about a row that worked.
 */
export function factNote(fact: ExecutionFact): string {
  if (fact.failure !== null) {
    return fact.attempt !== null && fact.attempt > 0
      ? `${fact.failure.kind} ×${fact.attempt + 1}`
      : fact.failure.kind;
  }
  if (fact.outputPort !== null && !RESERVED_PORTS.has(fact.outputPort)) {
    return `→ ${fact.outputPort}`;
  }
  return fact.occurrence > 0 ? `occurrence ${fact.occurrence + 1}` : "";
}

/**
 * §2.2's header count, from `run.factCount` and never from `facts.length` —
 * a truncated read returns fewer facts than the run has, and that difference is
 * exactly what the count exists to state.
 */
export function factCountSummary(count: FactCount): string {
  if (count.truncated) {
    return `showing ${thousands(count.returned)} of ${thousands(count.total)} · truncated`;
  }
  return `${thousands(count.returned)} fact${count.returned === 1 ? "" : "s"}, all`;
}

/**
 * Step 4's ctx chip, spelled as words: this node replaced the context document.
 *
 * It marks rows *within* a run that captures per-node context; on its own the
 * absence of a chip cannot separate "this node wrote nothing" from "this run
 * records nothing per node" (the `ContextLane` STEP-9 RISK). Which of the two
 * it is gets stated once, run-level, by the screen's CONTEXT section — move or
 * drop that section and this chip becomes ambiguous.
 */
function contextChip(fact: ExecutionFact): JSX.Element {
  // Replacement is whole-document, so only a non-null write replaced anything;
  // `final-only` and `absent` say nothing per node and so mark nothing.
  if (fact.context.mode !== "before-write-after" || fact.context.write === null) {
    return null;
  }
  return (
    <span class="execution-ctx frame">
      ctx<span class="visually-hidden"> replaced the context document</span>
    </span>
  );
}

const factColumns: ReadonlyArray<Column<ExecutionFact>> = [
  { header: "#", cell: (fact) => fact.index },
  { header: "node", cell: (fact) => fact.node },
  { header: "type", cell: (fact) => fact.nodeType },
  {
    header: "status",
    cell: (fact) => <StatusBadge status={fact.status} tone={nodeRunTone(fact.status)} />,
  },
  {
    header: "note",
    cell: (fact) => {
      const note = factNote(fact);
      const chip = contextChip(fact);
      // Separated in the text and not only by the chip's margin: the cell's
      // copied text, and the words a screen reader runs together, would
      // otherwise read `in-stockctx` on a row that both branched and wrote.
      return (
        <>
          {chip !== null && note !== "" ? `${note} ` : note}
          {chip}
        </>
      );
    },
  },
  {
    header: "duration",
    cell: (fact) =>
      // Null wherever the platform cannot time the node (STEP-9 RISK): the cell
      // renders the absence, because a zero would be a timing this run has not.
      fact.durationMs === null ? (
        <span class="execution-absent">
          —<span class="visually-hidden">not recorded</span>
        </span>
      ) : (
        `${thousands(fact.durationMs)} ms`
      ),
  },
];

export function ExecutionTable(props: {
  run: Run;
  inspector: (fact: ExecutionFact) => JSX.Element;
  /**
   * Trace mode's expand-all / collapse-all, as `RowDisclosure`'s controlled
   * mode: supplied, it drives every row; left undefined, each row keeps its
   * own state and the reader drives it.
   */
  open?: boolean;
  /**
   * What a controlled row asked for. A controlled row reports every toggle and
   * moves only when its owner answers, so an owner driving `open` needs this to
   * hear the click it has to answer.
   */
  onToggle?: (next: boolean) => void;
}): JSX.Element {
  return (
    <DataTable
      caption="execution facts, in returned order"
      columns={factColumns}
      rows={props.run.facts}
      rowKey={(fact) => String(fact.index)}
      // §2.2 tints error rows only, and takes the tint from the badge's own
      // tone so the edge can never say something the status word does not.
      tone={(fact) => (fact.status === "error" ? nodeRunTone(fact.status) : null)}
      disclosure={(fact) => ({
        label: `fact ${fact.index}, ${fact.node}`,
        open: props.open,
        onToggle: (next) => props.onToggle?.(next),
        content: () => props.inspector(fact),
      })}
      empty={<EmptyState region="execution" reason="this read returned no facts" />}
      footer={<span class="frame">{factCountSummary(props.run.factCount)}</span>}
    />
  );
}
