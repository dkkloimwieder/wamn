import { cleanup, fireEvent, render } from "@solidjs/testing-library";
import { flush } from "solid-js";
import { afterAll, afterEach, beforeAll, describe, expect, it } from "vitest";

import { readStatus, setReadStatus } from "../app/read-status";
import {
  CAPTURE_OFF_RUN_ID,
  EFFECT_UNCERTAIN_RUN_ID,
  FAILING_RUN_ID,
  PASSING_RUN_ID,
  TERMINALIZED_RUN_ID,
  TRUNCATED_RUN_ID,
  captureOffRun,
  effectUncertainRun,
  failingRun,
  passingRun,
  runFixtures,
  terminalizedRun,
  truncatedRun,
} from "../reader/fixtures";
import { EFFECT_UNCERTAIN_MEANING, type ExecutionFact, type Run } from "../reader/types";
import { RunScreen, foldRunContext, sharedPlanHash } from "./run-screen";

afterEach(() => {
  cleanup();
  // the dot is global state this screen drives, so each test starts from cold
  setReadStatus("never-contacted");
});

/**
 * Two run states the six fixtures do not hold, registered with the reader for
 * the length of this file. The screen takes an id and reads through
 * `selectReader()` — it has no reader of its own to stub — so a state that has
 * no fixture is reached by giving the fixture reader one.
 */
const FACTLESS_RUN_ID = "01J9RUNNINGWITHNOFACTSYETX";
const FAULT_RUN_ID = "01J9FAULTWHILERENDERINGXXX";

/** Reachable today: a run that is still running has no facts to return yet. */
const factlessRun: Run = {
  ...failingRun,
  runId: FACTLESS_RUN_ID,
  status: "running",
  failure: null,
  facts: [],
  factCount: { returned: 0, total: 0, truncated: false },
};

/** A value a part cannot render, so the screen's fault boundary is the one thing under test. */
const faultRun: Run = {
  ...failingRun,
  runId: FAULT_RUN_ID,
  get facts(): readonly ExecutionFact[] {
    throw new Error("the fact lane could not be read");
  },
};

const registry = runFixtures as Map<string, Run>;

beforeAll(() => {
  registry.set(FACTLESS_RUN_ID, factlessRun);
  registry.set(FAULT_RUN_ID, faultRun);
});

afterAll(() => {
  registry.delete(FACTLESS_RUN_ID);
  registry.delete(FAULT_RUN_ID);
});

/** Two ticks settle the reader's promise; `flush` lands the reactive work. */
async function settle(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
  flush();
}

async function screen(id: string) {
  const rendered = render(() => <RunScreen id={id} />);
  await settle();
  return rendered;
}

/**
 * The labels of the sections *this screen* opens, in order. Scoped to the
 * screen's own sections on purpose: a part the screen composes may open
 * headings of its own, and the order under test is the screen's argument.
 */
const labels = (container: HTMLElement): string[] =>
  [...container.querySelectorAll(".run-section > .section-label .section-label-text")].map(
    (label) => label.textContent ?? "",
  );

/**
 * What a reader reaches on a control: its label if it has one, otherwise its
 * text with every aria-hidden subtree removed. Spelled exactly as
 * `src/ui/accessibility-floor.test.tsx` spells it, so a pair of buttons this
 * suite calls distinct cannot fail the floor once the floor reaches this screen.
 */
function reachableText(element: Element): string {
  if (element.closest('[aria-hidden="true"]') !== null) {
    return "";
  }
  const label = element.getAttribute("aria-label");
  if (label !== null) {
    return label.trim();
  }
  const copy = element.cloneNode(true) as Element;
  for (const hidden of copy.querySelectorAll('[aria-hidden="true"]')) {
    hidden.remove();
  }
  return (copy.textContent ?? "").replace(/\s+/g, " ").trim();
}

/** A section by the label that opens it, so these read independently of order. */
function section(container: HTMLElement, label: string): HTMLElement {
  const found = [...container.querySelectorAll<HTMLElement>(".run-section")].find(
    (candidate) => candidate.querySelector(".section-label-text")?.textContent === label,
  );
  if (found === undefined) {
    throw new Error(`the screen has no ${label} section`);
  }
  return found;
}

const disclosed = (container: HTMLElement): HTMLElement[] => [
  ...container.querySelectorAll<HTMLElement>(".data-table-disclosed"),
];

/** One KeyValue's value, by its key — the pairs render as `dt`/`dd`, unspaced. */
function pair(root: HTMLElement, label: string): string {
  const found = [...root.querySelectorAll<HTMLElement>(".key-value")].find(
    (candidate) => candidate.querySelector(".key-value-label")?.textContent === label,
  );
  return found?.querySelector(".key-value-value")?.textContent ?? "";
}

describe("foldRunContext", () => {
  it("folds the wireframe run's one write to the document the run ended on", () => {
    const folded = foldRunContext(failingRun);
    expect(folded.mode).toBe("before-write-after");
    if (folded.mode !== "before-write-after") {
      throw new Error("the wireframe run captures before/write/after context");
    }
    expect(folded.final).toEqual({ order: { sku: "SKU-118", quantity: 2 } });
    // one fact replaced the document; the other four left it alone
    expect(folded.writes.map((fact) => fact.index)).toEqual([2]);
  });

  it("keeps the document the run began with when no fact replaced it", () => {
    const untouched: Run = {
      ...failingRun,
      facts: failingRun.facts.map((fact) => ({
        ...fact,
        context: {
          mode: "before-write-after",
          before: { order: "ord-4471" },
          write: null,
          after: { order: "ord-4471" },
        } as const,
      })),
    };

    const folded = foldRunContext(untouched);
    expect(folded).toEqual({
      mode: "before-write-after",
      final: { order: "ord-4471" },
      writes: [],
    });
  });

  it("does not fold the single-cell mode: one document is the whole run's", () => {
    expect(foldRunContext(passingRun)).toEqual({
      mode: "final-only",
      final: { order: "ord-4471", reserved: true },
    });
  });

  it("answers absent for every run that captured no context, and for no facts", () => {
    for (const run of [captureOffRun, truncatedRun, effectUncertainRun, terminalizedRun]) {
      expect(foldRunContext(run)).toEqual({ mode: "absent" });
    }
    expect(foldRunContext({ ...failingRun, facts: [] })).toEqual({ mode: "absent" });
  });
});

describe("sharedPlanHash", () => {
  it("answers the hash the returned facts agree on", () => {
    expect(sharedPlanHash(failingRun)).toBe(failingRun.facts[0].planHash);
    expect(sharedPlanHash(effectUncertainRun)).toBe(effectUncertainRun.facts[0].planHash);
  });

  it("answers nothing when the facts disagree, or when there are none", () => {
    const replanned: Run = {
      ...failingRun,
      facts: failingRun.facts.map((fact, position) =>
        position === 0 ? fact : { ...fact, planHash: `${fact.planHash}00` },
      ),
    };
    expect(sharedPlanHash(replanned)).toBeNull();
    expect(sharedPlanHash({ ...failingRun, facts: [] })).toBeNull();
  });
});

describe("RunScreen — the read", () => {
  it("stands on the loading block until the read settles", async () => {
    const { container } = render(() => <RunScreen id={FAILING_RUN_ID} />);

    expect(container).toHaveTextContent("loading the run");
    expect(container.querySelector("h1")).toBeNull();

    await settle();
    expect(container).not.toHaveTextContent("loading the run");
    expect(container.querySelector("h1")).toHaveTextContent(
      "FAILED · retry-exhausted at fetch-inventory",
    );
  });

  it("renders a refusal as the typed value it is, and keeps refresh reachable", async () => {
    const { container, getByRole } = await screen("01J9NOTHINGSTOREDHEREXX");

    expect(container.querySelector(".error-panel")).toHaveTextContent("not found");
    // the platform's literal, never rewritten
    expect(container).toHaveTextContent("run-not-found");
    expect(container).toHaveTextContent("01J9NOTHINGSTOREDHEREXX");
    // nothing was read, so nothing on the screen may claim to be a verdict
    expect(container.querySelector("h1")).toBeNull();
    getByRole("button", { name: /refresh this screen/ });
  });

  it("survives a read that lands after the reader has left the screen", async () => {
    render(() => <RunScreen id={FAILING_RUN_ID} />);
    // navigation disposes the screen mid-read; the answer still arrives
    cleanup();
    await settle();

    // nothing throws, and nothing moves: §1.4's dot describes the read the
    // author is waiting on, and nobody is waiting on this one
    expect(readStatus()).toBe("never-contacted");
  });

  it("drives §1.4's read status: ok on an answer, fail on a read that got none", async () => {
    await screen(FAILING_RUN_ID);
    expect(readStatus()).toBe("ok");
    cleanup();

    // a missing run is a 2xx answer, not an unreachable target: the screen
    // shows the refusal, and the dot may not report the console as out of touch
    await screen("01J9NOTHINGSTOREDHEREXX");
    expect(readStatus()).toBe("ok");
    cleanup();

    await screen("network-down");
    expect(readStatus()).toBe("fail");
    cleanup();

    await screen("refused-authorization");
    expect(readStatus()).toBe("fail");
  });

  it("states a fault in a part rather than blanking the screen", async () => {
    const { container } = await screen(FAULT_RUN_ID);

    expect(container.querySelector(".run-fault")).toHaveTextContent(
      "the run screen failed to render",
    );
    expect(container.querySelector(".run-fault")).toHaveTextContent(
      "the fact lane could not be read",
    );
    // the boundary is inside the screen, so the screen itself is still standing
    expect(container.querySelector(".run-screen")).toBeInTheDocument();
  });

  it("re-issues the one read from the one refresh control", async () => {
    const { container, getByRole } = await screen(FAILING_RUN_ID);

    const refresh = getByRole("button", { name: /refresh this screen/ });
    // §2.6's `refreshed 12:04:31 ↻`, stamped by the read that just settled
    expect(refresh).toHaveTextContent(/refreshed \d\d:\d\d:\d\d/);

    fireEvent.click(refresh);
    flush();
    // the read is in flight again: the control says so and cannot be issued twice,
    // and the answer already on the screen stays there rather than blanking
    expect(refresh).toBeDisabled();
    expect(container).not.toHaveTextContent("loading the run");

    await settle();
    expect(refresh).toBeEnabled();
    expect(container.querySelector("h1")).toHaveTextContent(
      "FAILED · retry-exhausted at fetch-inventory",
    );
    expect(readStatus()).toBe("ok");
  });
});

describe("RunScreen — §2.2's failing run", () => {
  it("reproduces the wireframe: verdict, run failure, execution", async () => {
    const { container } = await screen(FAILING_RUN_ID);

    expect(container.querySelector(".verdict-bar")).toHaveAttribute("data-tone", "fail");
    expect(container.querySelector("h1")).toHaveTextContent(
      "FAILED · retry-exhausted at fetch-inventory",
    );
    expect(container.querySelector(".verdict-identity")).toHaveTextContent("captured full");

    // §2.2's order is the design's argument, so the order is asserted
    expect(labels(container)).toEqual(["run failure", "execution", "context", "details"]);

    const failure = container.querySelector(".failure-panel");
    expect(failure).toHaveTextContent("retry-exhausted");
    expect(failure).toHaveTextContent("fetch-inventory");
    expect(failure).toHaveTextContent("upstream 503 after 4 attempts");

    expect(container.querySelectorAll("tbody .data-table-row")).toHaveLength(5);
    expect(container.querySelector("tfoot")).toHaveTextContent("5 facts, all");
    // §2.2's `EXECUTION ──── 14 facts, all`: the count is on the rule as well
    expect(section(container, "execution").querySelector(".section-label .run-count")).toHaveTextContent(
      "5 facts, all",
    );
  });

  it("carries the run-level facts that are not the verdict", async () => {
    const { container, getByRole } = await screen(FAILING_RUN_ID);
    const details = section(container, "details");

    // the bar ellipsizes the trace; the details section prints it whole
    expect(pair(details, "trace")).toBe(failingRun.traceId);
    getByRole("button", { name: `copy plan hash id ${failingRun.facts[0].planHash}` });
    expect(pair(details, "capture")).toBe("full");
    // one spelling of the draft on the screen, and it is the verdict bar's
    expect(pair(details, "draft")).toBe("orders@17");
    expect(container.querySelector(".run-identity")).toHaveTextContent("draft orders@17");
    expect(pair(details, "facts")).toBe("5 facts, all");
  });

  it("folds the run's context to the document it ended on", async () => {
    const { container } = await screen(FAILING_RUN_ID);
    const context = section(container, "context");

    expect(context).toHaveTextContent("1 of 5 returned facts replaced it");
    expect(context).toHaveTextContent("last at fact 2, parse-order");
    expect(context).toHaveTextContent("SKU-118");
    // one read, one fold: the section names what it is showing
    expect(context.querySelector(".json-view")).toBeInTheDocument();
  });

  it("opens one row onto the three-lane inspector", async () => {
    const { container, getByLabelText } = await screen(FAILING_RUN_ID);

    fireEvent.click(getByLabelText("expand fact 4, fetch-inventory"));
    flush();

    const rows = disclosed(container);
    expect(rows).toHaveLength(1);
    expect(rows[0]).toHaveTextContent("retryable");
    // the errored output is stated, never blanked
    expect(rows[0]).toHaveTextContent("the node failed, so there is no output");
    expect(rows[0]).toHaveTextContent("×4 of this visit");
  });
});

describe("RunScreen — the other five run fixtures", () => {
  it("replaces the run-failure section with the uncertain panel", async () => {
    const { container } = await screen(EFFECT_UNCERTAIN_RUN_ID);

    expect(container.querySelector(".verdict-bar")).toHaveAttribute("data-tone", "uncertain");
    // the panel *is* the section: it stands where run failure would, opens its
    // own headings, and no run-failure section is left on the screen
    expect(container.querySelector(".uncertain-panel")).toBeInTheDocument();
    expect(container.querySelector(".failure-panel")).toBeNull();
    expect(labels(container)).toEqual(["execution", "context", "details"]);

    const panel = container.querySelector(".uncertain-panel");
    for (const sentence of EFFECT_UNCERTAIN_MEANING) {
      expect(panel).toHaveTextContent(sentence);
    }
    expect(panel).toHaveTextContent("unresolved");
  });

  it("keeps the panel over a terminalized run, and shows the record", async () => {
    const { container } = await screen(TERMINALIZED_RUN_ID);

    // status `failed`, kind still `effect-uncertain`: the panel is still the section
    expect(container.querySelector("h1")).toHaveTextContent(
      "FAILED · effect-uncertain at charge-card",
    );
    expect(container.querySelector(".failure-panel")).toBeNull();

    const panel = container.querySelector(".uncertain-panel");
    expect(panel).toHaveTextContent("operator-terminalized");
    expect(panel).toHaveTextContent("counterparty-confirmation");
    expect(panel).toHaveTextContent("ops/2026-08-14/psp-settlement-report.csv");
    expect(panel).toHaveTextContent("operator-terminalized-effect-uncertain");
  });

  it("gives a passing run no failure section and one context document", async () => {
    const { container } = await screen(PASSING_RUN_ID);

    expect(container.querySelector("h1")).toHaveTextContent("SUCCEEDED");
    expect(labels(container)).toEqual(["execution", "context", "details"]);
    expect(container).toHaveTextContent("one document for the whole run: there is nothing to fold");
    expect(container).toHaveTextContent("ord-4471");
    expect(container.querySelector("tfoot")).toHaveTextContent(
      `${passingRun.factCount.returned} facts, all`,
    );
  });

  it("states the truncated run's withheld facts in both places that count them", async () => {
    const { container } = await screen(TRUNCATED_RUN_ID);

    expect(container.querySelectorAll("tbody .data-table-row")).toHaveLength(200);
    expect(container.querySelector("tfoot")).toHaveTextContent(
      "showing 200 of 3,412 · truncated",
    );
    expect(section(container, "execution").querySelector(".section-label .run-count")).toHaveTextContent(
      "showing 200 of 3,412 · truncated",
    );
    expect(pair(section(container, "details"), "facts")).toBe("showing 200 of 3,412 · truncated");
    // the 3,212 facts nobody saw could disagree, so the hash is claimed for the
    // window it was read across and not for the run
    expect(pair(section(container, "details"), "plan hash")).toContain(
      "agreed by the returned facts only",
    );
    // nothing was captured, so nothing can be folded — and it is not called empty
    expect(container).toHaveTextContent("this run captured no context");
    expect(container).not.toHaveTextContent("returned facts replaced it");
  });

  it("says what a run with no facts yet carries, rather than nothing", async () => {
    const { container } = await screen(FACTLESS_RUN_ID);
    const details = section(container, "details");

    expect(container.querySelector("h1")).toHaveTextContent("RUNNING");
    expect(section(container, "execution")).toHaveTextContent("this read returned no facts");
    expect(section(container, "execution").querySelector(".section-label .run-count")).toHaveTextContent(
      "0 facts, all",
    );
    expect(pair(details, "facts")).toBe("0 facts, all");
    // an empty read is not a disagreement: there was no hash to agree on
    expect(pair(details, "plan hash")).toBe("this read returned no facts, so it carries no plan hash");
    expect(section(container, "context")).toHaveTextContent("this run captured no context");
  });

  it("says capture was off rather than blanking the run's captured values", async () => {
    const { container, getByLabelText } = await screen(CAPTURE_OFF_RUN_ID);

    expect(pair(section(container, "details"), "capture")).toBe("off");

    fireEvent.click(getByLabelText("expand fact 1, ingress"));
    flush();
    // §2.2's words, on both captured lanes of the row
    expect(disclosed(container)[0].textContent).toContain("capture was off for this run");
    expect(
      [...disclosed(container)[0].querySelectorAll(".empty-state")].filter((state) =>
        (state.textContent ?? "").includes("capture was off"),
      ),
    ).toHaveLength(2);
    expect(container.querySelectorAll("tbody .data-table-row")).toHaveLength(
      captureOffRun.factCount.returned,
    );
  });
});

describe("RunScreen — trace mode", () => {
  it("opens every row at once, with the inspector compressed", async () => {
    const { container, getByRole } = await screen(FAILING_RUN_ID);

    expect(disclosed(container)).toHaveLength(0);
    fireEvent.click(getByRole("button", { name: "expand all execution rows" }));
    flush();

    expect(disclosed(container)).toHaveLength(failingRun.facts.length);
    // §6 step 4's two-line JSON, with the whole document one `[json]` pop away
    expect(container.querySelectorAll(".fact-json-preview").length).toBeGreaterThan(0);
    expect(container.querySelector(".data-table-disclosed .json-view")).toBeNull();
    expect(container).toHaveTextContent("[json] fact 1 input");
  });

  it("hands the rows back on collapse-all, and leaves the reader's own row open", async () => {
    const { container, getByLabelText, getByRole } = await screen(FAILING_RUN_ID);

    // a row the reader opened by hand, before trace mode ever ran
    fireEvent.click(getByLabelText("expand fact 3, check-stock"));
    flush();
    expect(disclosed(container)).toHaveLength(1);

    fireEvent.click(getByRole("button", { name: "expand all execution rows" }));
    flush();
    expect(disclosed(container)).toHaveLength(failingRun.facts.length);

    fireEvent.click(getByRole("button", { name: "collapse all execution rows" }));
    flush();
    // trace mode closed the rows it opened; the reader's row is still theirs
    expect(disclosed(container)).toHaveLength(1);
    // the row the reader opened, still open: fact 3 is the run's one taken branch
    expect(disclosed(container)[0]).toHaveTextContent("→ in-stock");
    // and out of trace mode it is the whole document again, not two lines
    expect(container.querySelector(".fact-json-preview")).toBeNull();
    expect(container.querySelector(".data-table-disclosed .json-view")).toBeInTheDocument();
  });

  it("hands the table back when a row is collapsed from inside trace mode", async () => {
    const { container, getByLabelText, getByRole } = await screen(FAILING_RUN_ID);

    fireEvent.click(getByLabelText("expand fact 1, ingress"));
    flush();
    fireEvent.click(getByRole("button", { name: "expand all execution rows" }));
    flush();
    expect(disclosed(container)).toHaveLength(failingRun.facts.length);

    // the toggle says `collapse fact 3`, so fact 3 is what collapses — a control
    // in the tab order may not name an act it then declines to perform
    fireEvent.click(getByLabelText("collapse fact 3, check-stock"));
    flush();
    expect(section(container, "execution")).not.toHaveTextContent("trace mode");
    expect(disclosed(container)).toHaveLength(1);
    getByLabelText("expand fact 3, check-stock");
    // and the row the reader opened by hand is still theirs, still open
    getByLabelText("collapse fact 1, ingress");
  });

  it("keeps trace mode while the reader opens a lane inside a row", async () => {
    const { container, getByRole } = await screen(FAILING_RUN_ID);
    fireEvent.click(getByRole("button", { name: "expand all execution rows" }));
    flush();

    // a disclosure inside a row is not a row toggle: popping fact 1's document
    // opens what it names and leaves the whole-run trace standing
    fireEvent.click(getByRole("button", { name: "[json] fact 1 input" }));
    flush();
    expect(section(container, "execution")).toHaveTextContent("trace mode");
    expect(disclosed(container)).toHaveLength(failingRun.facts.length);
  });

  it("keeps every control on the whole-run trace named for what it acts on", async () => {
    const { container, getByRole } = await screen(FAILING_RUN_ID);
    fireEvent.click(getByRole("button", { name: "expand all execution rows" }));
    flush();

    // §3's floor, on the one screen state that puts every control on at once:
    // two controls that act on two different things may not share a name.
    const names = [...container.querySelectorAll("button")].map(reachableText);
    expect(names.filter((name) => !/\p{L}/u.test(name))).toEqual([]);
    expect(new Set(names).size).toBe(names.length);
  });

  it("says in words which state the table is in", async () => {
    const { container, getByRole } = await screen(FAILING_RUN_ID);
    const execution = section(container, "execution");

    expect(execution).not.toHaveTextContent("trace mode");
    // the region is mounted before it has anything to say, which is the only
    // way the change gets announced rather than silently inserted
    const spoken = execution.querySelector('[aria-live="polite"]');
    expect(spoken).toBeInTheDocument();

    fireEvent.click(getByRole("button", { name: "expand all execution rows" }));
    flush();
    expect(spoken).toHaveTextContent("trace mode");
    expect(execution).toHaveTextContent("trace mode");

    fireEvent.click(getByRole("button", { name: "collapse all execution rows" }));
    flush();
    expect(execution).not.toHaveTextContent("trace mode");
    // one screen, one heading: the verdict (wamn-dggp.21)
    expect(container.querySelectorAll("h1")).toHaveLength(1);
  });
});
