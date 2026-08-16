import { cleanup, fireEvent, render } from "@solidjs/testing-library";
import { flush } from "solid-js";
import { afterEach, describe, expect, it } from "vitest";

import { FAILING_RUN_ID, allPassReport, finalizedReport } from "../reader/fixtures";
import type { Report, ReportCase } from "../reader/types";
import { ReportCases } from "./report-cases";

/**
 * §2.3's case list: pass rows are one quiet line, failed rows arrive open, and
 * `run →` is the handoff — including where the report did not carry one.
 */

afterEach(cleanup);

const casesOf = (report: Report): readonly ReportCase[] =>
  report.state === "finalized" ? report.cases : [];

function list(cases: readonly ReportCase[]) {
  const rendered = render(() => <ReportCases cases={cases} />);
  flush();
  return rendered;
}

const disclosed = (container: HTMLElement): HTMLElement[] => [
  ...container.querySelectorAll<HTMLElement>(".data-table-disclosed"),
];

describe("which rows arrive open", () => {
  it("opens the failed rows and leaves the passed ones closed", () => {
    const { container, getByLabelText } = list(casesOf(finalizedReport));

    // one failing case in the fixture, and it is the only row already open
    expect(disclosed(container)).toHaveLength(1);
    getByLabelText("collapse case backorders-when-out-of-stock");
    getByLabelText("expand case creates-order");

    // the disclosure a passed row keeps still opens
    fireEvent.click(getByLabelText("expand case creates-order"));
    flush();
    expect(disclosed(container)).toHaveLength(2);
  });

  it("leaves every row closed when every case passed", () => {
    const { container } = list(casesOf(allPassReport));

    expect(disclosed(container)).toHaveLength(0);
    expect(container.querySelectorAll("tbody .data-table-row")).toHaveLength(12);
  });

  it("shows the failed case's kind and every failed assertion", () => {
    const { container } = list(casesOf(finalizedReport));
    const detail = disclosed(container)[0];

    expect(detail).toHaveTextContent("assertion-failed");
    // one failure per family, each as an expected/observed pair
    expect(detail.querySelectorAll(".report-assertion")).toHaveLength(4);
    expect(detail).toHaveTextContent("named-node-terminal");
    expect(detail).toHaveTextContent("run-terminal-outcome");
    expect(detail).toHaveTextContent("typed-flow-failure");
    expect(detail).toHaveTextContent("terminal-respond");
  });

  it("says what a passed case carries rather than opening onto nothing", () => {
    const { container, getByLabelText } = list(casesOf(finalizedReport));

    fireEvent.click(getByLabelText("expand case rejects-empty-cart"));
    flush();

    // a `ReportCase` holds only its failed assertions, so a passing case has no
    // detail at all — which the open row states instead of looking broken
    const opened = disclosed(container).find((row) =>
      (row.textContent ?? "").includes("this case passed"),
    );
    expect(opened).toBeDefined();
    expect(opened).toHaveTextContent("records no assertion detail for a case that passed");
  });

  it("does not borrow that wording for a case that failed without failing an assertion", () => {
    // reachable today: `deadline-exhausted` is a case failure kind that fails no
    // assertion, so the row has a kind and an empty assertion list
    const { container } = list([
      {
        caseId: "times-out-on-slow-inventory",
        passed: false,
        runId: null,
        failureKind: "deadline-exhausted",
        failedAssertions: [],
      },
    ]);

    expect(disclosed(container)[0]).toHaveTextContent("deadline-exhausted");
    expect(disclosed(container)[0]).toHaveTextContent(
      "this case failed, and the report lists no failed assertion for it",
    );
    expect(container).not.toHaveTextContent("this case passed");
  });
});

describe("the run handoff", () => {
  it("links each case to its own run", () => {
    const { container } = list(casesOf(finalizedReport));
    const links = [...container.querySelectorAll("a")];

    expect(links).toHaveLength(12);
    // §5's acceptance journey: the failing case points at the run this reader serves
    expect(links.map((link) => link.getAttribute("href"))).toContain(`#/run/${FAILING_RUN_ID}`);
    // the visible text is §2.3's `run →`; which run it opens is what a reader
    // reaching the link alone would otherwise not be told
    expect(links[0].textContent).toContain("run");
    expect(links[0]).toHaveTextContent("creates-order");
  });

  it("says the report carried no run id rather than rendering a dead link", () => {
    const { container } = list(casesOf(allPassReport));

    // the fixture's last case carries none — the link is durable but no wire
    // field exposes it, so this is the state step 9 will actually hit
    expect(container.querySelectorAll("a")).toHaveLength(11);
    expect(container).toHaveTextContent("no run id recorded");
    for (const link of container.querySelectorAll("a")) {
      expect(link.getAttribute("href")).not.toContain("null");
    }
  });
});

describe("the floor the list has to keep", () => {
  it("captions the table and names every toggle by the case it opens", () => {
    const { container } = list(casesOf(finalizedReport));

    expect(container.querySelector("table > caption")).toHaveTextContent("test cases");

    // one toggle per row, plus the controls the open row's evidence brings with
    // it — every one of them named for what it acts on, and no two alike
    const names = [...container.querySelectorAll("button")].map(
      (button) => button.getAttribute("aria-label") ?? "",
    );
    expect(names.filter((name) => !/\p{L}/u.test(name))).toEqual([]);
    expect(new Set(names).size).toBe(names.length);
    expect(names).toContain("expand case creates-order");
    expect(names).toContain("collapse case backorders-when-out-of-stock");
  });

  it("tells two bodies of evidence on one row apart", () => {
    // A case can fail more than one assertion of the same family, and each
    // `terminal-respond` brings a JsonView with a copy button and expanders of
    // its own. Named by the case alone they would all be one name, which is the
    // duplicate this floor forbids — so the assertion's position is in the name.
    const { container } = list([
      {
        caseId: "answers-both-legs",
        passed: false,
        runId: null,
        failureKind: "assertion-failed",
        failedAssertions: [
          {
            expected: { family: "terminal-respond", status: 201, body: { created: true } },
            observed: "terminal response status 500 did not match 201",
          },
          {
            expected: { family: "terminal-respond", status: 204, body: { released: true } },
            observed: "terminal response status 500 did not match 204",
          },
        ],
      },
    ]);

    const names = [...container.querySelectorAll("button")].map(
      (button) => button.getAttribute("aria-label") ?? "",
    );
    // one copy button per body, and neither can be mistaken for the other
    const copies = names.filter((name) => name.startsWith("copy "));
    expect(copies).toHaveLength(2);
    expect(new Set(copies).size).toBe(2);
    for (const name of copies) {
      expect(name).toContain("answers-both-legs");
    }
    expect(new Set(names).size).toBe(names.length);
  });

  it("tells a failed row in a word, not only by the edge it tints", () => {
    const { container } = list(casesOf(finalizedReport));
    const tinted = [...container.querySelectorAll("tbody tr[data-tone]")];

    expect(tinted).toHaveLength(1);
    expect(tinted[0]).toHaveTextContent("failed");
    expect(tinted[0]).toHaveTextContent("backorders-when-out-of-stock");
  });

  it("owes a reason even when the report finalized with no cases at all", () => {
    const { container } = list([]);

    expect(container).toHaveTextContent("this report finalized with no cases");
  });
});
