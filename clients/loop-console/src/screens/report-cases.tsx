import type { JSX } from "@solidjs/web";
import { For, Show } from "solid-js";

import type { ReportCase } from "../reader/types";
import { toHash } from "../routing/route";
import { DataTable, type Column } from "../ui/data-table";
import { KeyValue } from "../ui/key-value";
import { PassGlyph } from "../ui/pass-glyph";
import { EmptyState } from "../ui/states";
import { AssertionFailureView } from "./report-assertion";
import "./report-cases.css";

/**
 * §2.3's case list: *which case failed → on what expectation → take me to its
 * run*, as a configuration of the step-3 `DataTable`.
 *
 * A pass row is one quiet line — glyph, name, run link — and a failed row
 * arrives open, because the author came for it. The report never shows per-node
 * execution beyond the assertion's own selector line; `run →` is the handoff.
 */

/**
 * §2.3's `run →`.
 *
 * The case→run link is durable (`authoring_test_case_runs.run_id`) but no wire
 * field exposes it, so a null run id is the state step 9 will actually hit. It
 * reads as the absence it is: a link to nowhere would send the author to a
 * not-found, and a blank cell would claim the case had no run.
 */
function CaseRunLink(props: { reportCase: ReportCase }): JSX.Element {
  return (
    <Show
      when={props.reportCase.runId}
      fallback={<span class="report-case-absent frame">no run id recorded</span>}
    >
      {(runId) => (
        <a class="report-run-link" href={toHash({ kind: "run", id: runId() })}>
          {/* Every row's link says `run →`; which run it opens is what tells two
              of them apart, so the case name rides along for a reader who
              reaches the link without its row. */}
          run<span class="visually-hidden"> for {props.reportCase.caseId}</span>{" "}
          <span aria-hidden="true">→</span>
        </a>
      )}
    </Show>
  );
}

const caseColumns: ReadonlyArray<Column<ReportCase>> = [
  { header: "result", cell: (reportCase) => <PassGlyph passed={reportCase.passed} /> },
  { header: "case", cell: (reportCase) => reportCase.caseId },
  { header: "run", cell: (reportCase) => <CaseRunLink reportCase={reportCase} /> },
];

/**
 * Why a disclosed case has no assertions to show. Two states, two sentences: a
 * `ReportCase` carries `failedAssertions` and nothing else, so a passing case
 * has no detail to render at all — and a case can fail without failing an
 * assertion (`deadline-exhausted`, `effect-uncertain`), which is a different
 * fact and may not borrow the other's wording.
 */
function noAssertions(reportCase: ReportCase): string {
  return reportCase.passed
    ? "this case passed, and the report records no assertion detail for a case that passed"
    : "this case failed, and the report lists no failed assertion for it";
}

function CaseDetail(props: { reportCase: ReportCase }): JSX.Element {
  return (
    <div class="report-case-detail">
      {/* Null exactly when the case passed, so the pair appears only on a failure. */}
      <Show when={props.reportCase.failureKind}>
        {(kind) => <KeyValue label="failure">{kind()}</KeyValue>}
      </Show>
      <Show
        when={props.reportCase.failedAssertions.length > 0}
        fallback={
          <EmptyState
            region={`case ${props.reportCase.caseId}`}
            reason={noAssertions(props.reportCase)}
          />
        }
      >
        <For each={props.reportCase.failedAssertions}>
          {(failure, index) => (
            <AssertionFailureView
              failure={failure}
              subject={`case ${props.reportCase.caseId} assertion ${index() + 1}`}
            />
          )}
        </For>
      </Show>
    </div>
  );
}

export function ReportCases(props: { cases: readonly ReportCase[] }): JSX.Element {
  return (
    <DataTable
      caption="test cases, in report order"
      columns={caseColumns}
      rows={props.cases}
      rowKey={(reportCase) => reportCase.caseId}
      // The tint restates the glyph's own tone, which already says `failed` in a
      // word — the edge finds the row for the eye, it never carries the state.
      tone={(reportCase) => (reportCase.passed ? null : "fail")}
      disclosure={(reportCase) => ({
        label: `case ${reportCase.caseId}`,
        // §2.3: failed rows auto-expand, passed rows keep a disclosure that
        // starts closed — the pass row's job is to stay one quiet line.
        startOpen: !reportCase.passed,
        content: () => <CaseDetail reportCase={reportCase} />,
      })}
      empty={<EmptyState region="cases" reason="this report finalized with no cases" />}
    />
  );
}
