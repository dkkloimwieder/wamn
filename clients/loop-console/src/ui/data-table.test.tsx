import { cleanup, fireEvent, render } from "@solidjs/testing-library";
import { For, createSignal, flush } from "solid-js";
import { afterEach, describe, expect, it } from "vitest";

import { captureOffRun, failingRun, finalizedReport, truncatedRun } from "../reader/fixtures";
import type { ExecutionFact, ReportCase } from "../reader/types";
import { DataTable, type Column } from "./data-table";
import { KeyValue } from "./key-value";
import { PassGlyph } from "./pass-glyph";
import { EmptyState } from "./states";
import { nodeRunTone } from "./status";
import { StatusBadge } from "./status-badge";

afterEach(cleanup);

/** §2.2's note column: the one scrap per row that usually answers "what happened". */
function note(fact: ExecutionFact): string {
  if (fact.failure !== null) {
    const attempts = (fact.attempt ?? 0) + 1;
    return attempts > 1 ? `${fact.failure.kind} ×${attempts}` : fact.failure.kind;
  }
  return fact.outputPort !== null && fact.outputPort !== "main" ? `→ ${fact.outputPort}` : "";
}

/** §2.2's execution table, as step 4 will build it. */
const factColumns: ReadonlyArray<Column<ExecutionFact>> = [
  { header: "#", cell: (fact) => fact.index },
  { header: "node", cell: (fact) => fact.node },
  { header: "type", cell: (fact) => fact.nodeType },
  {
    header: "status",
    cell: (fact) => <StatusBadge status={fact.status} tone={nodeRunTone(fact.status)} />,
  },
  { header: "note", cell: (fact) => note(fact) },
];

const factKey = (fact: ExecutionFact): string => String(fact.index);

const noFacts = <EmptyState region="execution" reason="this read returned no facts" />;

/** §2.3's case list: the pass glyph, the case name, and its run. */
const caseColumns: ReadonlyArray<Column<ReportCase>> = [
  { header: "result", cell: (reportCase) => <PassGlyph passed={reportCase.passed} /> },
  { header: "case", cell: (reportCase) => reportCase.caseId },
  { header: "run", cell: (reportCase) => reportCase.runId ?? "—" },
];

const reportCases: readonly ReportCase[] =
  finalizedReport.state === "finalized" ? finalizedReport.cases : [];

function rows(container: HTMLElement): HTMLElement[] {
  return [...container.querySelectorAll<HTMLElement>("tbody .data-table-row")];
}

describe("DataTable", () => {
  it("renders the failing run's facts as a captioned table, in returned order", () => {
    const { container } = render(() => (
      <DataTable
        caption="execution facts"
        columns={factColumns}
        rows={failingRun.facts}
        rowKey={factKey}
        empty={noFacts}
      />
    ));

    expect(container.querySelector("caption")).toHaveTextContent("execution facts");
    expect([...container.querySelectorAll("thead th")].map((head) => head.textContent)).toEqual([
      "#",
      "node",
      "type",
      "status",
      "note",
    ]);
    for (const head of container.querySelectorAll("thead th")) {
      expect(head).toHaveAttribute("scope", "col");
    }

    const body = rows(container);
    expect(body).toHaveLength(failingRun.facts.length);
    expect(body.map((row) => row.querySelectorAll("td")[1].textContent)).toEqual(
      failingRun.facts.map((fact) => fact.node),
    );
    // the two scraps §2.2 says the note column has to carry
    expect(body[2]).toHaveTextContent("→ in-stock");
    expect(body[3]).toHaveTextContent("retryable ×4");
  });

  it("tints the failing run's error rows, which still say their status in words", () => {
    const { container } = render(() => (
      <DataTable
        caption="execution facts"
        columns={factColumns}
        rows={failingRun.facts}
        rowKey={factKey}
        tone={(fact) => (fact.status === "error" ? "fail" : null)}
        empty={noFacts}
      />
    ));

    const tinted = [...container.querySelectorAll<HTMLElement>(".data-table-row[data-tone]")];
    expect(tinted).toHaveLength(2);
    for (const row of tinted) {
      expect(row).toHaveAttribute("data-tone", "fail");
      expect(row).toHaveTextContent("error");
    }
    // and no untinted row was quietly claimed as an error
    expect(rows(container).filter((row) => row.hasAttribute("data-tone"))).toHaveLength(2);
    cleanup();

    // the tone is whatever the caller returned, not a fixed `fail`: §2.0's tree
    // and §2.3's cases reach for the other four
    const every = render(() => (
      <DataTable
        caption="execution facts"
        columns={factColumns}
        rows={failingRun.facts}
        rowKey={factKey}
        tone={(fact) => nodeRunTone(fact.status)}
        empty={noFacts}
      />
    ));
    expect(rows(every.container).map((row) => row.getAttribute("data-tone"))).toEqual(
      failingRun.facts.map((fact) => nodeRunTone(fact.status)),
    );
  });

  it("discloses a row into a row of its own, spanning the columns", () => {
    const { container, getByLabelText } = render(() => (
      <DataTable
        caption="execution facts"
        columns={factColumns}
        rows={failingRun.facts}
        rowKey={factKey}
        disclosure={(fact) => ({
          label: `fact ${fact.index}, ${fact.node}`,
          content: () => (
            <p>
              occurrence {fact.occurrence + 1} · frame{" "}
              {fact.parentFrameId === null ? "root" : fact.frameId}
            </p>
          ),
        })}
        empty={noFacts}
      />
    ));

    const toggle = getByLabelText("expand fact 4, fetch-inventory");
    // a native button, so §2.6's Enter and Space come free
    expect(toggle.tagName).toBe("BUTTON");
    expect(toggle).not.toHaveAttribute("tabindex");
    expect(toggle).toHaveAttribute("aria-expanded", "false");
    // in the first cell, where §2.2's `#` column puts it ahead of the row's data
    expect(rows(container)[3].querySelectorAll("td")[0]).toContainElement(toggle);
    expect(container.querySelector(".data-table-disclosed")).toBeNull();

    fireEvent.click(toggle);
    flush();

    expect(getByLabelText("collapse fact 4, fetch-inventory")).toHaveAttribute(
      "aria-expanded",
      "true",
    );
    const disclosed = container.querySelector<HTMLElement>(".data-table-disclosed");
    expect(disclosed).toHaveAttribute("id", toggle.getAttribute("aria-controls"));
    expect(disclosed).toHaveTextContent("occurrence 1 · frame root");
    expect(disclosed?.querySelector("td")).toHaveAttribute("colspan", "5");
    // it belongs to the row above it, and only that row opened
    expect(disclosed?.previousElementSibling).toBe(rows(container)[3]);
    expect(container.querySelectorAll(".data-table-disclosed")).toHaveLength(1);

    fireEvent.click(toggle);
    flush();
    expect(container.querySelector(".data-table-disclosed")).toBeNull();
  });

  it("keeps an open disclosure with its row when a re-read returns the facts reordered", () => {
    const [facts, setFacts] = createSignal(failingRun.facts);
    const { container, getByLabelText } = render(() => (
      <DataTable
        caption="execution facts"
        columns={factColumns}
        rows={facts()}
        rowKey={factKey}
        disclosure={(fact) => ({
          label: `fact ${fact.index}, ${fact.node}`,
          content: () => <p>occurrence {fact.occurrence + 1}</p>,
        })}
        empty={noFacts}
      />
    ));

    fireEvent.click(getByLabelText("expand fact 4, fetch-inventory"));
    flush();
    expect(container.querySelectorAll(".data-table-disclosed")).toHaveLength(1);

    // §2.6's refresh re-issues the read: the same facts by index, but new
    // objects in a new order, so only `rowKey` can carry the open row across
    setFacts([...failingRun.facts].reverse().map((fact) => ({ ...fact })));
    flush();

    expect(getByLabelText("collapse fact 4, fetch-inventory")).toHaveAttribute(
      "aria-expanded",
      "true",
    );
    const disclosed = container.querySelector<HTMLElement>(".data-table-disclosed");
    expect(container.querySelectorAll(".data-table-disclosed")).toHaveLength(1);
    expect(disclosed?.previousElementSibling).toBe(rows(container)[1]);
    expect(rows(container)[1]).toHaveTextContent("fetch-inventory");
  });

  it("offers no toggle on a row whose disclosure the caller declines", () => {
    const { container, getAllByRole } = render(() => (
      <DataTable
        caption="execution facts"
        columns={factColumns}
        rows={failingRun.facts}
        rowKey={factKey}
        // only the error rows have a node failure to open
        disclosure={(fact) =>
          fact.failure === null
            ? null
            : {
                label: `fact ${fact.index} failure`,
                content: () => <p>{fact.failure?.detail}</p>,
              }
        }
        empty={noFacts}
      />
    ));

    expect(getAllByRole("button")).toHaveLength(2);
    expect(rows(container)[0].querySelector("button")).toBeNull();
    expect(rows(container)[3].querySelector("button")).not.toBeNull();
  });

  it("starts the report's failed case open and leaves the passed cases closed", () => {
    const { container, getAllByRole } = render(() => (
      <DataTable
        caption="cases"
        columns={caseColumns}
        rows={reportCases}
        rowKey={(reportCase) => reportCase.caseId}
        tone={(reportCase) => (reportCase.passed ? null : "fail")}
        disclosure={(reportCase) => ({
          label: `${reportCase.caseId} assertions`,
          // §2.3: failed rows auto-expand — the author came for them
          startOpen: !reportCase.passed,
          content: () => (
            <For each={reportCase.failedAssertions}>
              {(failed) => (
                <>
                  <KeyValue label="expected">{failed.expected.family}</KeyValue>
                  <KeyValue label="observed">{failed.observed}</KeyValue>
                </>
              )}
            </For>
          ),
        })}
        empty={<EmptyState region="cases" reason="this report finalized with no cases" />}
      />
    ));

    expect(rows(container)).toHaveLength(reportCases.length);
    const disclosed = [...container.querySelectorAll<HTMLElement>(".data-table-disclosed")];
    expect(disclosed).toHaveLength(1);
    expect(disclosed[0]).toHaveTextContent("named-node-terminal");
    expect(disclosed[0]).toHaveTextContent(
      "run terminal status Failed did not match Completed",
    );
    expect(disclosed[0].previousElementSibling).toHaveTextContent("backorders-when-out-of-stock");

    // the failed row's glyph says it too, so the tint is never the only carrier
    expect(disclosed[0].previousElementSibling).toHaveTextContent("failed");
    expect(getAllByRole("button", { name: /^expand /u })).toHaveLength(reportCases.length - 1);
  });

  it("carries the truncated run's footer beside its 200 returned rows", () => {
    const count = truncatedRun.factCount;
    const { container } = render(() => (
      <DataTable
        caption="execution facts"
        columns={factColumns}
        rows={truncatedRun.facts}
        rowKey={factKey}
        empty={noFacts}
        footer={
          <span class="frame">
            showing {count.returned} of {count.total.toLocaleString("en-US")}
            {count.truncated ? " · truncated" : ""}
          </span>
        }
      />
    ));

    expect(rows(container)).toHaveLength(count.returned);
    expect(container.querySelector("tfoot")).toHaveTextContent("showing 200 of 3,412 · truncated");
    expect(container.querySelector("tfoot td")).toHaveAttribute("colspan", "5");
  });

  it("renders the capture-off run's output slot as an empty state, not a blank", () => {
    const { container } = render(() => (
      <DataTable
        caption="execution facts"
        columns={factColumns}
        rows={captureOffRun.facts}
        rowKey={factKey}
        disclosure={(fact) => ({
          label: `fact ${fact.index}, ${fact.node}`,
          startOpen: true,
          content: () =>
            fact.output.state === "capture-off" ? (
              <EmptyState
                region="output"
                reason={`capture was ${captureOffRun.capture} for this run`}
              />
            ) : (
              <p>output</p>
            ),
        })}
        empty={noFacts}
      />
    ));

    const disclosed = container.querySelectorAll(".data-table-disclosed");
    expect(disclosed).toHaveLength(captureOffRun.facts.length);
    for (const row of disclosed) {
      expect(row).toHaveTextContent("capture was off for this run");
      // every disclosed row follows its own row, never another row's disclosure
      expect(row.previousElementSibling).toHaveClass("data-table-row");
    }
  });

  it("renders the empty slot across the columns when a read returns no rows", () => {
    const none: readonly ExecutionFact[] = [];
    const { container } = render(() => (
      <DataTable
        caption="execution facts"
        columns={factColumns}
        rows={none}
        rowKey={factKey}
        empty={noFacts}
      />
    ));

    expect(rows(container)).toHaveLength(0);
    const cell = container.querySelector("tbody td");
    expect(cell).toHaveAttribute("colspan", "5");
    expect(cell).toHaveTextContent("this read returned no facts");
    // the caption and the columns still frame what is missing
    expect(container.querySelector("caption")).toHaveTextContent("execution facts");
    expect(container.querySelectorAll("thead th")).toHaveLength(factColumns.length);
  });
});
