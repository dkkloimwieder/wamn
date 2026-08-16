import { cleanup, fireEvent, render } from "@solidjs/testing-library";
import type { JSX } from "@solidjs/web";
import { createSignal, flush } from "solid-js";
import { afterEach, describe, expect, it } from "vitest";

import {
  captureOffRun,
  effectUncertainRun,
  failingRun,
  passingRun,
  terminalizedRun,
  truncatedRun,
} from "../reader/fixtures";
import type { ExecutionFact, Run } from "../reader/types";
import { ExecutionTable, factCountSummary, factNote } from "./execution-table";

afterEach(cleanup);

/** Stands in for the inspector lane: enough to prove the row opened onto it. */
const inspector = (fact: ExecutionFact): JSX.Element => <p>inspecting {fact.node}</p>;

const table = (run: Run, open?: boolean) =>
  render(() => <ExecutionTable run={run} inspector={inspector} open={open} />);

const bodyRows = (container: HTMLElement): HTMLElement[] => [
  ...container.querySelectorAll<HTMLElement>("tbody .data-table-row"),
];

const column = (container: HTMLElement, index: number): string[] =>
  bodyRows(container).map((row) => row.querySelectorAll("td")[index].textContent ?? "");

/** The `#` cell also carries the row's disclosure toggle; the index is the rest of it. */
const indexColumn = (container: HTMLElement): string[] =>
  bodyRows(container).map((row) =>
    [...row.querySelectorAll("td")[0].childNodes]
      .filter((node) => node.nodeName !== "BUTTON")
      .map((node) => node.textContent ?? "")
      .join(""),
  );

describe("factNote", () => {
  it("says nothing on the passing run, whose every fact took `main` on its first visit", () => {
    expect(passingRun.facts.map(factNote)).toEqual(["", "", "", ""]);
  });

  it("carries the wireframe's branch and retry count on the failing run", () => {
    // §2.2 verbatim: the taken branch on fact 3, `retryable ×4` on fact 4, and
    // `terminal` on fact 5 — whose second visit is outranked by its failure.
    expect(failingRun.facts.map(factNote)).toEqual([
      "",
      "",
      "→ in-stock",
      "retryable ×4",
      "terminal",
    ]);
  });

  it("prints no count for a fact whose attempt nothing recorded", () => {
    // The uncertain run's effectful fact is still `started`: no failure, no port.
    expect(effectUncertainRun.facts.map(factNote)).toEqual(["", ""]);
    // Terminalized, it has a failure and an attempt of null — which must never
    // become the `×1` that would claim a retry the platform never recorded.
    expect(terminalizedRun.facts.map(factNote)).toEqual(["", "terminal"]);
    expect(terminalizedRun.facts[1].attempt).toBeNull();
  });

  it("distinguishes the truncated run's 200 visits of one node by occurrence", () => {
    expect(truncatedRun.facts.map(factNote)).toEqual([
      "",
      ...Array.from({ length: 199 }, (_, position) => `occurrence ${position + 2}`),
    ]);
  });

  it("says nothing extra when capture is off — capture removes values, not facts", () => {
    expect(captureOffRun.facts.map(factNote)).toEqual(["", "", ""]);
  });

  it("drops the count on a retry that ended in success — `×N` only ever rides a failure", () => {
    expect(factNote({ ...passingRun.facts[1], attempt: 2 })).toBe("");
  });
});

describe("factCountSummary", () => {
  it("counts the whole run when nothing was truncated", () => {
    expect(factCountSummary(failingRun.factCount)).toBe("5 facts, all");
    expect(factCountSummary({ returned: 1, total: 1, truncated: false })).toBe("1 fact, all");
  });

  it("states what was withheld when the read was truncated", () => {
    expect(factCountSummary(truncatedRun.factCount)).toBe("showing 200 of 3,412 · truncated");
  });
});

describe("ExecutionTable", () => {
  it("reproduces §2.2's table over the failing run", () => {
    const { container } = table(failingRun);

    expect(container.querySelector("caption")).toHaveTextContent(
      "execution facts, in returned order",
    );
    expect([...container.querySelectorAll("thead th")].map((head) => head.textContent)).toEqual([
      "#",
      "node",
      "type",
      "status",
      "note",
      "duration",
    ]);

    expect(bodyRows(container)).toHaveLength(5);
    expect(indexColumn(container)).toEqual(["1", "2", "3", "4", "5"]);
    expect(column(container, 1)).toEqual([
      "ingress",
      "parse-order",
      "check-stock",
      "fetch-inventory",
      "fetch-inventory",
    ]);
    expect(column(container, 2)).toEqual([
      "request",
      "transform",
      "conditional",
      "http-request",
      "http-request",
    ]);
    expect(column(container, 3)).toEqual(["success", "success", "success", "error", "error"]);
    expect(column(container, 4)).toEqual([
      "",
      "ctx replaced the context document",
      "→ in-stock",
      "retryable ×4",
      "terminal",
    ]);
  });

  it("numbers a row by the fact's own index, never by its position on the page", () => {
    // A truncated read is a page of a longer run, and its first row is not fact 1.
    const { container } = table({
      ...truncatedRun,
      facts: truncatedRun.facts
        .slice(0, 3)
        .map((fact, position) => ({ ...fact, index: 3201 + position })),
      factCount: { returned: 3, total: 3412, truncated: true },
    });

    expect(indexColumn(container)).toEqual(["3201", "3202", "3203"]);
  });

  it("tints the failing run's two error rows, which still say `error` in words", () => {
    const { container } = table(failingRun);

    const tinted = bodyRows(container).filter((row) => row.hasAttribute("data-tone"));
    expect(tinted).toHaveLength(2);
    for (const row of tinted) {
      expect(row).toHaveAttribute("data-tone", "fail");
      expect(row).toHaveTextContent("error");
    }
  });

  it("leaves the uncertain run's outstanding row untinted — it has not failed", () => {
    const { container } = table(effectUncertainRun);

    expect(bodyRows(container).filter((row) => row.hasAttribute("data-tone"))).toHaveLength(0);
    expect(column(container, 3)).toEqual(["success", "started"]);
  });

  it("tints the terminalized row, which failed without ever being timed", () => {
    const { container } = table(terminalizedRun);

    expect(bodyRows(container)).toHaveLength(2);
    expect(column(container, 3)).toEqual(["success", "error"]);

    const tinted = bodyRows(container).filter((row) => row.hasAttribute("data-tone"));
    expect(tinted).toHaveLength(1);
    expect(tinted[0]).toHaveAttribute("data-tone", "fail");
    // Tint and absence on one row: the kind with no `×`, and no timing at all.
    expect(column(container, 4)).toEqual(["", "terminal"]);
    expect(column(container, 5)).toEqual(["2 ms", "—not recorded"]);
  });

  it("marks only the fact that replaced the context document", () => {
    const written = table(failingRun);
    const chips = written.container.querySelectorAll<HTMLElement>(".execution-ctx");
    expect(chips).toHaveLength(1);
    // a word to a screen reader, never a glyph alone
    expect(chips[0]).toHaveTextContent("ctx replaced the context document");
    expect(bodyRows(written.container)[1]).toContainElement(chips[0]);
    cleanup();

    // `final-only` is one document for the whole run: no fact of it wrote.
    expect(table(passingRun).container.querySelectorAll(".execution-ctx")).toHaveLength(0);
    cleanup();
    // and `absent` — the truthful mode today — marks nothing at all.
    expect(table(captureOffRun).container.querySelectorAll(".execution-ctx")).toHaveLength(0);
  });

  it("separates the note from the chip in the text, not only in the margin", () => {
    // No fixture has one fact both branch and write; the state is reachable, and
    // run together the cell would read `in-stockctx` to a reader copying it out.
    const { container } = table({
      ...failingRun,
      facts: [
        {
          ...failingRun.facts[2],
          context: {
            mode: "before-write-after",
            before: {},
            write: { order: "ord-4471" },
            after: { order: "ord-4471" },
          },
        },
      ],
      factCount: { returned: 1, total: 1, truncated: false },
    });

    expect(column(container, 4)).toEqual(["→ in-stock ctx replaced the context document"]);
  });

  it("renders an untimed fact as an absence rather than a zero", () => {
    const timed = table(failingRun);
    expect(column(timed.container, 5)).toEqual(["2 ms", "1 ms", "1 ms", "1,204 ms", "318 ms"]);
    cleanup();

    // Nothing in the capture-off run is timed: `durationMs` is null on every
    // fact the platform cannot time, which today is every ordinary node.
    const untimed = table(captureOffRun);
    expect(column(untimed.container, 5)).toEqual([
      "—not recorded",
      "—not recorded",
      "—not recorded",
    ]);
    expect(untimed.container.querySelector("tbody")).not.toHaveTextContent("0 ms");
  });

  it("footers the count from the run, not from the rows it returned", () => {
    const whole = table(failingRun);
    expect(whole.container.querySelector("tfoot")).toHaveTextContent("5 facts, all");
    cleanup();

    const truncated = table(truncatedRun);
    expect(bodyRows(truncated.container)).toHaveLength(200);
    expect(truncated.container.querySelector("tfoot")).toHaveTextContent(
      "showing 200 of 3,412 · truncated",
    );
  });

  it("says why a run with no facts shows none, and still counts what it has", () => {
    // `queued`, `dispatched` and `running` have no fixture and no facts yet: the
    // read succeeded and returned nothing, which is not the same as failing.
    const { container } = table({
      ...passingRun,
      status: "queued",
      facts: [],
      factCount: { returned: 0, total: 0, truncated: false },
    });

    expect(bodyRows(container)).toHaveLength(0);
    expect(container.querySelector("tbody")).toHaveTextContent(
      "execution: this read returned no facts",
    );
    expect(container.querySelector("tfoot")).toHaveTextContent("0 facts, all");
  });

  it("opens a row onto the inspector, under a name that says which row", () => {
    const { container, getByLabelText } = table(failingRun);

    expect(container.querySelectorAll(".data-table-disclosed")).toHaveLength(0);
    // the two `fetch-inventory` facts are separate rows and separate names
    const toggle = getByLabelText("expand fact 5, fetch-inventory");
    fireEvent.click(toggle);
    flush();

    const disclosed = container.querySelectorAll(".data-table-disclosed");
    expect(disclosed).toHaveLength(1);
    expect(disclosed[0]).toHaveTextContent("inspecting fetch-inventory");
    expect(disclosed[0].previousElementSibling).toBe(bodyRows(container)[4]);
  });

  it("opens every row for trace mode", () => {
    const { container } = table(failingRun, true);

    const disclosed = [...container.querySelectorAll(".data-table-disclosed")];
    expect(disclosed).toHaveLength(failingRun.facts.length);
    expect(disclosed.map((row) => row.textContent)).toEqual(
      failingRun.facts.map((fact) => `inspecting ${fact.node}`),
    );
  });

  it("follows trace mode both ways after it is mounted", () => {
    const [open, setOpen] = createSignal<boolean | undefined>(undefined);
    const { container } = render(() => (
      <ExecutionTable run={truncatedRun} inspector={inspector} open={open()} />
    ));

    expect(container.querySelectorAll(".data-table-disclosed")).toHaveLength(0);
    // expand-all reaches rows that were mounted closed — the 200-row trace
    setOpen(true);
    flush();
    expect(container.querySelectorAll(".data-table-disclosed")).toHaveLength(200);

    setOpen(false);
    flush();
    expect(container.querySelectorAll(".data-table-disclosed")).toHaveLength(0);
  });
});
