import { cleanup, fireEvent, render } from "@solidjs/testing-library";
import { flush } from "solid-js";
import { afterEach, describe, expect, it, vi } from "vitest";

import { readStatus, setReadStatus } from "../app/read-status";
import { focusAnchor, screenContribution } from "../app/screen-actions";
import { err, ok, type ReadResult } from "../reader/errors";
import { fixtureReader } from "../reader/fixture-reader";
import {
  ALL_PASS_REPORT_ID,
  FAILING_RUN_ID,
  FINALIZED_REPORT_ID,
  PENDING_REPORT_ID,
  allPassReport,
  finalizedReport,
  pendingReport,
} from "../reader/fixtures";
import type { Report } from "../reader/types";
import { clearVisited, visitedOfKind } from "../store/visited";
import { ReportScreen, reportVerdictLine, reportVerdictTone } from "./report-screen";

/**
 * §2.3's screen, over the route's own path — an id into `<ReportScreen>`, which
 * reads through `selectReader()` — because what is under test is the screen an
 * author reaches, not a part handed a fixture.
 */

afterEach(() => {
  cleanup();
  // both are global state this screen drives, so each test starts from cold
  setReadStatus("never-contacted");
  clearVisited();
  // the stale-read test holds the fixture reader open; nothing may leak forward
  vi.restoreAllMocks();
});

/** Two ticks settle the reader's promise; `flush` lands the reactive work. */
async function settle(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
  flush();
}

async function screen(id: string) {
  const rendered = render(() => <ReportScreen id={id} />);
  await settle();
  return rendered;
}

describe("reportVerdictLine · reportVerdictTone", () => {
  it("answers §2.3's `N / M PASSED`, toned by whether the set passed", () => {
    expect(reportVerdictLine(finalizedReport)).toBe("11 / 12 PASSED");
    expect(reportVerdictTone(finalizedReport)).toBe("fail");
    expect(reportVerdictLine(allPassReport)).toBe("12 / 12 PASSED");
    expect(reportVerdictTone(allPassReport)).toBe("ok");
  });

  it("states pending as the state word, and never as a reason it was not given", () => {
    expect(reportVerdictLine(pendingReport)).toBe("PENDING");
    expect(reportVerdictTone(pendingReport)).toBe("warn");
    // the branch a projection that grows a reason would take — the line prints
    // what it was handed, and only what it was handed
    const withReason: Report = {
      state: "pending",
      reportId: pendingReport.reportId,
      testSetHash: pendingReport.testSetHash,
      pendingReason: "the reservation has not been claimed",
    };
    expect(reportVerdictLine(withReason)).toBe("PENDING · the reservation has not been claimed");
  });

  it("will not call a finalized report with nothing in it a pass", () => {
    // No fixture reaches this, and the read can: both counts are synthesized by
    // the reader out of the untyped `summary` (types.ts STEP-9 RISK), so a
    // finalized report with no cases is a shape step 9 can hand the screen.
    const empty: Report = {
      state: "finalized",
      reportId: FINALIZED_REPORT_ID,
      testSetHash: finalizedReport.testSetHash,
      passed: 0,
      total: 0,
      cases: [],
    };

    // the answer §1.3 puts before the author's eye may not be a green all-pass
    // over a set that never ran, nor contradict the list under it
    expect(reportVerdictLine(empty)).toBe("NO CASES RECORDED");
    expect(reportVerdictLine(empty)).not.toContain("PASSED");
    expect(reportVerdictTone(empty)).toBe("neutral");
  });
});

describe("ReportScreen — the read", () => {
  it("stands on the loading block until the read settles", async () => {
    const { container } = render(() => <ReportScreen id={FINALIZED_REPORT_ID} />);

    expect(container).toHaveTextContent("loading the report");
    expect(container.querySelector("h1")).toBeNull();

    await settle();
    expect(container).not.toHaveTextContent("loading the report");
    expect(container.querySelector("h1")).toHaveTextContent("11 / 12 PASSED");
  });

  it("renders a refusal as the typed value it is, and keeps refresh reachable", async () => {
    const { container, getByRole } = await screen("01J9NOTHINGSTOREDHEREXX");

    expect(container.querySelector(".error-panel")).toHaveTextContent("not found");
    // the platform's literal, never rewritten
    expect(container).toHaveTextContent("report-not-found");
    // nothing was read, so nothing on the screen may claim to be a verdict
    expect(container.querySelector("h1")).toBeNull();
    getByRole("button", { name: /refresh this screen/ });
    expect(screenContribution().actions.map((action) => action.id)).toEqual(["refresh"]);
  });

  it("drives §1.4's read status: a missing report is an answer, a refused read is not", async () => {
    await screen(FINALIZED_REPORT_ID);
    expect(readStatus()).toBe("ok");
    cleanup();

    // a 2xx refusal carrying a literal: the platform answered, so the dot may
    // not report the console as out of touch
    await screen("01J9NOTHINGSTOREDHEREXX");
    expect(readStatus()).toBe("ok");
    cleanup();

    await screen("network-down");
    expect(readStatus()).toBe("fail");
    cleanup();

    await screen("refused-authorization");
    expect(readStatus()).toBe("fail");
  });

  it("records the visit only for a report the platform answered with", async () => {
    await screen(FINALIZED_REPORT_ID);
    expect(visitedOfKind("report")).toEqual([
      expect.objectContaining({ id: FINALIZED_REPORT_ID, verdict: "fail" }),
    ]);
    cleanup();

    // a refusal names no entity to remember
    await screen("01J9NOTHINGSTOREDHEREXX");
    expect(visitedOfKind("report")).toHaveLength(1);
    cleanup();

    await screen(ALL_PASS_REPORT_ID);
    expect(visitedOfKind("report")[0]).toMatchObject({ id: ALL_PASS_REPORT_ID, verdict: "ok" });
  });

  it("re-issues the one read from the one refresh control", async () => {
    const { container, getByRole } = await screen(FINALIZED_REPORT_ID);

    const refresh = getByRole("button", { name: /refresh this screen/ });
    expect(refresh).toHaveTextContent(/refreshed \d\d:\d\d:\d\d/);

    fireEvent.click(refresh);
    flush();
    // the read is in flight again, and the answer already on the screen stays
    expect(refresh).toBeDisabled();
    expect(container).not.toHaveTextContent("loading the report");

    await settle();
    expect(refresh).toBeEnabled();
    expect(container.querySelector("h1")).toHaveTextContent("11 / 12 PASSED");
  });

  it("survives a read that lands after the reader has left the screen", async () => {
    render(() => <ReportScreen id={FINALIZED_REPORT_ID} />);
    // navigation disposes the screen mid-read; the answer still arrives
    cleanup();
    await settle();

    // nothing throws, and nothing moves: §1.4's dot describes the read the
    // author is waiting on, and the reader's history is where they have been —
    // neither belongs to a screen they already left
    expect(readStatus()).toBe("never-contacted");
    expect(visitedOfKind("report")).toEqual([]);
  });

  it("lets an older answer overwrite neither the newer one nor §1.4's dot", async () => {
    const { container } = await screen(FINALIZED_REPORT_ID);

    // Two reads in flight at once, answered in the wrong order. Reachable: the
    // palette holds the same refresh callback the control does (§6 step 7) and
    // has no idea the control is busy, so the keyboard can issue the second.
    const answers: Array<(result: ReadResult<Report>) => void> = [];
    vi.spyOn(fixtureReader.reports, "get").mockImplementation(
      () =>
        new Promise<ReadResult<Report>>((resolve) => {
          answers.push(resolve);
        }),
    );
    const refresh = screenContribution().actions[0];
    refresh.run();
    flush();
    refresh.run();
    flush();
    expect(answers).toHaveLength(2);

    // the newer read lands first, and it is the answer the author is waiting on
    answers[1](ok(allPassReport));
    await settle();
    expect(container.querySelector("h1")).toHaveTextContent("12 / 12 PASSED");
    expect(readStatus()).toBe("ok");

    // the older one lands behind it and moves nothing: the dot would otherwise
    // report a read the author has already been answered past
    answers[0](
      err({ kind: "network", message: "authoring request failed before receiving an HTTP response" }),
    );
    await settle();
    expect(readStatus()).toBe("ok");
    expect(container.querySelector("h1")).toHaveTextContent("12 / 12 PASSED");
    expect(visitedOfKind("report")[0]).toMatchObject({ id: ALL_PASS_REPORT_ID });
  });

  it("offers the palette the cases section, and the section can be reached", async () => {
    const { container } = await screen(FINALIZED_REPORT_ID);

    expect(screenContribution().anchors).toEqual([{ id: "report-cases", label: "cases" }]);
    expect(container.querySelector("#report-cases")).toHaveAttribute("tabindex", "-1");
    expect(focusAnchor("report-cases")).toBe(true);
  });
});

describe("ReportScreen — §2.3's three verdicts", () => {
  it("reproduces the wireframe: 11 / 12 PASSED, fail-toned, over the case list", async () => {
    const { container } = await screen(FINALIZED_REPORT_ID);

    expect(container.querySelector(".verdict-bar")).toHaveAttribute("data-tone", "fail");
    expect(container.querySelector("h1")).toHaveTextContent("11 / 12 PASSED");

    // §2.3's identity line: the report, the test set, and the state word
    const identity = container.querySelector(".verdict-identity");
    expect(identity).toHaveTextContent("report");
    expect(identity).toHaveTextContent("test-set");
    expect(identity).toHaveTextContent("finalized");

    expect(container.querySelector(".section-label-text")).toHaveTextContent("cases");
    expect(container.querySelectorAll("tbody .data-table-row")).toHaveLength(12);
    // §2.3: the failed row arrives open, and its run link is the handoff
    expect(container.querySelectorAll(".data-table-disclosed")).toHaveLength(1);
    expect(
      [...container.querySelectorAll("a")].map((link) => link.getAttribute("href")),
    ).toContain(`#/run/${FAILING_RUN_ID}`);
  });

  it("tones an all-pass report ok, and still gives its verdict in words", async () => {
    const { container } = await screen(ALL_PASS_REPORT_ID);

    expect(container.querySelector(".verdict-bar")).toHaveAttribute("data-tone", "ok");
    expect(container.querySelector("h1")).toHaveTextContent("12 / 12 PASSED");
    expect(container.querySelectorAll(".data-table-disclosed")).toHaveLength(0);
  });

  it("tones a pending report warn and invents no reason for it", async () => {
    const { container } = await screen(PENDING_REPORT_ID);

    expect(container.querySelector(".verdict-bar")).toHaveAttribute("data-tone", "warn");
    expect(container.querySelector("h1")).toHaveTextContent("PENDING");
    expect(container.querySelector(".verdict-identity")).toHaveTextContent("pending");

    // §2.3 asks for the reason as the verdict line; the pending read carries
    // none, so the absence is stated and the line stays the state word alone
    expect(container.querySelector("h1")?.textContent).toBe("PENDING");
    expect(container).toHaveTextContent("no reason was recorded for this report being pending");

    // and there is no case list to draw, which is not the same as an empty one
    expect(container.querySelector("table")).toBeNull();
    expect(container).toHaveTextContent("this report has not finalized, so it records no cases yet");
    expect(container).not.toHaveTextContent("0 / 0");
  });
});

describe("ReportScreen — the floor, on the assembled screen", () => {
  it("names every button by what it acts on, and no two alike", async () => {
    const { container } = await screen(FINALIZED_REPORT_ID);

    const names = [...container.querySelectorAll("button")].map((button) => {
      const label = button.getAttribute("aria-label");
      if (label !== null) {
        return label.trim();
      }
      const copy = button.cloneNode(true) as Element;
      for (const hidden of copy.querySelectorAll('[aria-hidden="true"]')) {
        hidden.remove();
      }
      return (copy.textContent ?? "").replace(/\s+/g, " ").trim();
    });

    expect(names.filter((name) => !/\p{L}/u.test(name))).toEqual([]);
    expect(new Set(names).size).toBe(names.length);
    // one screen, one heading: the verdict (wamn-dggp.21)
    expect(container.querySelectorAll("h1")).toHaveLength(1);
  });
});
