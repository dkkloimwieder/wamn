import { cleanup, fireEvent, render } from "@solidjs/testing-library";
import { flush } from "solid-js";
import { afterEach, describe, expect, it } from "vitest";

import { setReadStatus } from "../app/read-status";
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
  terminalizedRun,
  truncatedRun,
} from "../reader/fixtures";
import { EFFECT_UNCERTAIN_MEANING, type Run } from "../reader/types";
import { RunScreen } from "./run-screen";

/**
 * §6 step 4's done-check, proved: *all run fixtures render correctly; the §2.2
 * wireframe and the whole-run trace are reproduced against fixtures.*
 *
 * Every rendering below goes through the route's own path — an id into
 * `<RunScreen>`, which reads through `selectReader()` — rather than handing a
 * component a fixture object, because "renders correctly" is a claim about the
 * screen the author reaches, not about a part in isolation.
 *
 * Where §2.2's wireframe and the fixtures disagree, the fixture wins and the
 * header of `reader/fixtures.ts` says why. Each such line is asserted as it is
 * true and the divergence named beside it, so the disagreement stays visible
 * instead of being quietly rounded off in either direction.
 */

afterEach(() => {
  cleanup();
  setReadStatus("never-contacted");
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

/** What the screen draws: the text with every screen-reader-only aside removed. */
function visible(element: Element): string {
  const copy = element.cloneNode(true) as Element;
  for (const aside of copy.querySelectorAll('.visually-hidden, [aria-hidden="true"]')) {
    aside.remove();
  }
  return (copy.textContent ?? "").replace(/\s+/g, " ").trim();
}

/** One row as the wireframe prints it: each cell's visible text, minus its toggle. */
function rowText(row: Element): string[] {
  return [...row.querySelectorAll(".data-table-cell")].map((cell) => {
    const copy = cell.cloneNode(true) as Element;
    for (const toggle of copy.querySelectorAll(".data-table-toggle")) {
      toggle.remove();
    }
    return visible(copy);
  });
}

const bodyRows = (container: HTMLElement): HTMLElement[] => [
  ...container.querySelectorAll<HTMLElement>("tbody .data-table-row"),
];

const disclosed = (container: HTMLElement): HTMLElement[] => [
  ...container.querySelectorAll<HTMLElement>(".data-table-disclosed"),
];

/** A section by the label that opens it. */
function section(container: HTMLElement, label: string): HTMLElement {
  const found = [...container.querySelectorAll<HTMLElement>(".run-section")].find(
    (candidate) => candidate.querySelector(".section-label-text")?.textContent === label,
  );
  if (found === undefined) {
    throw new Error(`the screen has no ${label} section`);
  }
  return found;
}

/** One KeyValue's value, by its key. */
function pair(root: HTMLElement, label: string): string {
  const found = [...root.querySelectorAll<HTMLElement>(".key-value")].find(
    (candidate) => candidate.querySelector(".key-value-label")?.textContent === label,
  );
  if (found === undefined) {
    throw new Error(`no pair is keyed ${label}`);
  }
  return visible(found.querySelector(".key-value-value") ?? found);
}

// ── every fixture, through the route ───────────────────────────────────────

/**
 * The six run fixtures and the answer each one's verdict bar owes, spelled out
 * here rather than taken from `runVerdictLine` or `runTone`: a done-check that
 * asks the screen's own functions what the screen should say proves only that
 * it is consistent with itself.
 */
const everyRunFixture: readonly { run: Run; tone: string; verdict: string }[] = [
  { run: passingRun, tone: "ok", verdict: "SUCCEEDED" },
  { run: failingRun, tone: "fail", verdict: "FAILED · retry-exhausted at fetch-inventory" },
  { run: effectUncertainRun, tone: "uncertain", verdict: "EFFECT-UNCERTAIN at charge-card" },
  { run: terminalizedRun, tone: "fail", verdict: "FAILED · effect-uncertain at charge-card" },
  { run: truncatedRun, tone: "ok", verdict: "SUCCEEDED" },
  { run: captureOffRun, tone: "ok", verdict: "SUCCEEDED" },
];

describe("the run screen, against every run fixture", () => {
  it("answers each fixture id with its own verdict, tone, and returned facts", async () => {
    for (const { run, tone, verdict } of everyRunFixture) {
      const { container } = await screen(run.runId);

      expect(container.querySelector(".run-fault")).toBeNull();
      const bar = container.querySelector(".verdict-bar");
      expect(bar).toHaveAttribute("data-tone", tone);
      // §1.1: the tone is said in its own name on the rule, never colour alone
      expect(container.querySelector(".verdict-rule")).toHaveTextContent(tone);
      expect(visible(container.querySelector("h1") as Element)).toBe(verdict);
      expect(bodyRows(container)).toHaveLength(run.factCount.returned);

      cleanup();
    }
  });
});

// ── §2.2's wireframe, line by line ─────────────────────────────────────────

describe("§2.2's wireframe over the failing run", () => {
  it("draws the verdict bar's three lines", async () => {
    const { container, getByRole } = await screen(FAILING_RUN_ID);

    // ▮▮▮ fail
    expect(container.querySelector(".verdict-bar")).toHaveAttribute("data-tone", "fail");
    expect(visible(container.querySelector(".verdict-rule") as Element)).toBe("fail");

    // FAILED · retry-exhausted at fetch-inventory
    expect(visible(container.querySelector("h1") as Element)).toBe(
      "FAILED · retry-exhausted at fetch-inventory",
    );

    // run 01J9…3F2K ⧉ · draft orders@17 → · trace 4bf9c1… ⧉ · captured full
    const identity = container.querySelector(".verdict-identity") as HTMLElement;
    expect(getByRole("button", { name: `copy run id ${FAILING_RUN_ID}` })).toHaveTextContent(
      "01J9…3F2K",
    );
    const draft = identity.querySelector("a") as HTMLAnchorElement;
    expect(draft).toHaveAttribute("href", "#/draft/orders/17");
    expect(visible(draft)).toBe("draft orders@17");
    // §2.6 fixes the ellipsis at first and last four, so the wireframe's own
    // `4bf9c1…` is the one truncation on this line the console does not draw.
    expect(getByRole("button", { name: `copy trace id ${failingRun.traceId}` })).toHaveTextContent(
      "4bf9…d8f3",
    );
    expect(visible(identity)).toContain("captured full");
  });

  it("orders the screen by the questions §2.2 asks", async () => {
    const { container } = await screen(FAILING_RUN_ID);

    // did it work → why not → what happened, step by step — then the two
    // run-level sections §6 step 4 adds beneath them
    expect(
      [...container.querySelectorAll(".run-section > .section-label .section-label-text")].map(
        (label) => label.textContent,
      ),
    ).toEqual(["run failure", "execution", "context", "details"]);
    // §1.3: one screen, one verdict, and the verdict is the screen's heading
    expect(container.querySelectorAll("h1")).toHaveLength(1);
  });

  it("draws the RUN FAILURE pairs", async () => {
    const { container } = await screen(FAILING_RUN_ID);
    const failure = section(container, "run failure");

    // kind        retry-exhausted
    expect(pair(failure, "kind")).toBe("retry-exhausted");
    // at          fetch-inventory (http-request)
    // — the node alone: `RunFailure` carries no node type, and the panel is
    // handed the failure rather than the fact list it could be joined from.
    expect(pair(failure, "at")).toBe("fetch-inventory");
    // detail      upstream 503 after 4 attempts        ▸ raw
    expect(pair(failure, "detail")).toBe("upstream 503 after 4 attempts");
    expect(failure.querySelector(".disclosure-toggle")).toHaveTextContent("raw failure");
  });

  it("draws the EXECUTION header count on the rule", async () => {
    const { container } = await screen(FAILING_RUN_ID);

    // EXECUTION ──────── 14 facts, all
    // — the wireframe's header counts fourteen facts over a five-row body; the
    // five rows are the run, so the truthful count is what the screen states.
    expect(section(container, "execution").querySelector(".run-count")).toHaveTextContent(
      "5 facts, all",
    );
    expect(container.querySelector("tfoot")).toHaveTextContent("5 facts, all");
  });

  it("draws the five rows, each with its node, type, status and note", async () => {
    const { container } = await screen(FAILING_RUN_ID);

    // #  node               type          status   note
    // — plus `duration`, which §6 step 4 adds to the columns §2.2 draws.
    expect(
      [...container.querySelectorAll(".data-table-head")].map((head) => head.textContent),
    ).toEqual(["#", "node", "type", "status", "note", "duration"]);

    expect(bodyRows(container).map(rowText)).toEqual([
      // 1  ingress            request       success
      ["1", "ingress", "request", "success", "", "2 ms"],
      // 2  parse-order        transform     success
      // — the note carries step 4's ctx chip: this fact is the run's one write.
      ["2", "parse-order", "transform", "success", "ctx", "1 ms"],
      // 3  check-stock        conditional   success   → in-stock
      ["3", "check-stock", "conditional", "success", "→ in-stock", "1 ms"],
      // 4  fetch-inventory    http-request  error     retryable ×4
      ["4", "fetch-inventory", "http-request", "error", "retryable ×4", "1,204 ms"],
      // 5  fetch-inventory    http-request  error     terminal
      ["5", "fetch-inventory", "http-request", "error", "terminal", "318 ms"],
    ]);

    // §2.2 tints the error rows and nothing else
    expect(bodyRows(container).map((row) => row.getAttribute("data-tone"))).toEqual([
      null,
      null,
      null,
      "fail",
      "fail",
    ]);
  });

  it("opens the retried row onto what the wireframe hangs under it", async () => {
    const { container, getByLabelText } = await screen(FAILING_RUN_ID);

    fireEvent.click(getByLabelText("expand fact 4, fetch-inventory"));
    flush();
    const row = disclosed(container)[0];

    // ├ occurrence 1–4 · frame root
    // — a retry never produces a second fact, so `occurrence 1–4` is
    // unproducible: the four attempts are this visit's `attempt`, and the
    // second `fetch-inventory` row is a second visit.
    expect(pair(row, "occurrence")).toBe("0 visit 1 of 2 returned for this node");
    expect(pair(row, "attempt")).toBe("3 ×4 of this visit");
    expect(pair(row, "frame")).toBe("0 root");
    // ├ failure  retryable · upstream 503        ▸ detail
    expect(pair(row, "failure")).toBe("retryable · upstream 503");
    // └ output  — (errored)
    expect(pair(row, "output")).toContain("the node failed, so there is no output");
  });
});

// ── the whole-run trace ────────────────────────────────────────────────────

describe("the whole-run trace", () => {
  it("opens every row at once, compressed, and hands them all back", async () => {
    const { container, getByRole } = await screen(FAILING_RUN_ID);

    expect(disclosed(container)).toHaveLength(0);
    fireEvent.click(getByRole("button", { name: "expand all execution rows" }));
    flush();

    // every row's inspector, open at once
    const open = disclosed(container);
    expect(open).toHaveLength(failingRun.facts.length);
    for (const row of open) {
      expect(row.querySelector(".fact-inspector")).toBeInTheDocument();
    }
    expect(section(container, "execution")).toHaveTextContent("trace mode");

    // §6 step 4's compressed rendering: two lines of JSON, the whole document a
    // `[json]` pop away, and `after unchanged` in place of a reprint.
    expect(container.querySelectorAll(".fact-json-preview").length).toBeGreaterThan(0);
    expect(container.querySelector(".data-table-disclosed .json-view")).toBeNull();
    getByRole("button", { name: "[json] fact 1 input" });
    expect(
      [...container.querySelectorAll(".fact-note")].filter(
        (note) => note.textContent === "after unchanged",
      ),
    ).toHaveLength(4);

    fireEvent.click(getByRole("button", { name: "collapse all execution rows" }));
    flush();
    expect(disclosed(container)).toHaveLength(0);
    expect(section(container, "execution")).not.toHaveTextContent("trace mode");
  });

  it("traces a two-hundred-row read without dropping the count it was read under", async () => {
    const { container, getByRole } = await screen(TRUNCATED_RUN_ID);

    fireEvent.click(getByRole("button", { name: "expand all execution rows" }));
    flush();

    expect(disclosed(container)).toHaveLength(truncatedRun.factCount.returned);
    expect(section(container, "execution").querySelector(".run-count")).toHaveTextContent(
      "showing 200 of 3,412 · truncated",
    );
  });
});

// ── the state the screen exists to protect ─────────────────────────────────

describe("the effect-uncertain run", () => {
  it("goes purple, replaces the failure section, and prints the meaning verbatim", async () => {
    const { container } = await screen(EFFECT_UNCERTAIN_RUN_ID);

    expect(container.querySelector(".verdict-bar")).toHaveAttribute("data-tone", "uncertain");
    // the panel *is* the section: no run-failure section is left standing
    const panel = container.querySelector(".uncertain-panel") as HTMLElement;
    expect(panel).toHaveAttribute("data-tone", "uncertain");
    expect(container.querySelector(".failure-panel")).toBeNull();

    for (const sentence of EFFECT_UNCERTAIN_MEANING) {
      expect([...panel.querySelectorAll(".uncertain-sentence")].map(visible)).toContain(sentence);
    }
    expect(panel).toHaveTextContent("unresolved");
    expect(panel).toHaveTextContent("no operator action is recorded for this run");
  });

  it("does not let the outstanding effect read as errored", async () => {
    const { container, getByLabelText } = await screen(EFFECT_UNCERTAIN_RUN_ID);

    // the effectful row is still `started`, and neither tinted nor failed
    expect(bodyRows(container).map(rowText)).toEqual([
      ["1", "ingress", "request", "success", "", "2 ms"],
      ["2", "charge-card", "http-request", "started", "", "—"],
    ]);
    expect(bodyRows(container)[1].getAttribute("data-tone")).toBeNull();

    fireEvent.click(getByLabelText("expand fact 2, charge-card"));
    flush();
    const row = disclosed(container)[0];

    expect(pair(row, "output")).toContain("the outcome is unknown — nothing was recorded either way");
    expect(row).not.toHaveTextContent("the node failed");
    // nothing durable was written for this visit, so not even the attempt survives
    expect(pair(row, "attempt")).toBe("not recorded");
  });
});

describe("the terminalized run", () => {
  it("keeps the panel over the resolved record, triple intact", async () => {
    const { container } = await screen(TERMINALIZED_RUN_ID);

    // status `failed`, kind still `effect-uncertain`, and a terminal reason:
    // the resolved discriminator, all three of it
    expect(visible(container.querySelector("h1") as Element)).toBe(
      "FAILED · effect-uncertain at charge-card",
    );
    const panel = container.querySelector(".uncertain-panel") as HTMLElement;
    expect(container.querySelector(".failure-panel")).toBeNull();
    expect(panel).toHaveTextContent("operator-terminalized");
    expect(pair(panel, "reason")).toBe("operator-terminalized-effect-uncertain");
    expect(pair(panel, "basis")).toBe("counterparty-confirmation");
    expect(pair(panel, "evidence")).toBe("ops/2026-08-14/psp-settlement-report.csv");
    expect(pair(panel, "operator")).toBe("wamn_project_admin");
    expect(pair(panel, "when")).toBe("2026-08-14T11:41:02Z");
    // the platform's meaning is not retired by the resolution
    for (const sentence of EFFECT_UNCERTAIN_MEANING) {
      expect([...panel.querySelectorAll(".uncertain-sentence")].map(visible)).toContain(sentence);
    }
  });

  it("shows the same node, now resolved rather than outstanding", async () => {
    const { container, getByLabelText } = await screen(TERMINALIZED_RUN_ID);

    expect(bodyRows(container).map(rowText)).toEqual([
      ["1", "ingress", "request", "success", "", "2 ms"],
      ["2", "charge-card", "http-request", "error", "terminal", "—"],
    ]);

    fireEvent.click(getByLabelText("expand fact 2, charge-card"));
    flush();
    const row = disclosed(container)[0];
    expect(pair(row, "failure")).toBe("terminal · operator terminalized unresolved effect");
    expect(pair(row, "output")).toContain("the node failed, so there is no output");
  });
});

// ── the other fixtures ─────────────────────────────────────────────────────

describe("the passing, truncated and capture-off runs", () => {
  it("gives the passing run no failure section at all", async () => {
    const { container } = await screen(PASSING_RUN_ID);

    expect(container.querySelector(".failure-panel")).toBeNull();
    expect(container.querySelector(".uncertain-panel")).toBeNull();
    expect(bodyRows(container)).toHaveLength(passingRun.factCount.returned);
    expect(bodyRows(container).map((row) => row.getAttribute("data-tone"))).toEqual([
      null,
      null,
      null,
      null,
    ]);
  });

  it("counts the truncated read truthfully in every place that counts it", async () => {
    const { container } = await screen(TRUNCATED_RUN_ID);

    expect(bodyRows(container)).toHaveLength(200);
    // §2.2: `showing 200 of 3,412 · truncated`
    expect(section(container, "execution").querySelector(".run-count")).toHaveTextContent(
      "showing 200 of 3,412 · truncated",
    );
    expect(container.querySelector("tfoot")).toHaveTextContent("showing 200 of 3,412 · truncated");
    expect(pair(section(container, "details"), "facts")).toBe("showing 200 of 3,412 · truncated");
  });

  it("says capture was off in §2.2's words, in every slot that has no value", async () => {
    const { container, getByLabelText } = await screen(CAPTURE_OFF_RUN_ID);

    expect(pair(section(container, "details"), "capture")).toBe("off");

    for (const fact of captureOffRun.facts) {
      fireEvent.click(getByLabelText(`expand fact ${fact.index}, ${fact.node}`));
    }
    flush();

    const rows = disclosed(container);
    expect(rows).toHaveLength(captureOffRun.facts.length);
    for (const row of rows) {
      // §2.2's exact words, on the output slot and on the input beside it
      expect(pair(row, "output")).toContain("capture was off for this run");
      expect(pair(row, "input")).toBe("capture was off for this run");
    }
  });
});

// ── the reader's other answers ─────────────────────────────────────────────

describe("the read's error and loading paths", () => {
  it("stands on the loading block until the read settles", async () => {
    const { container } = render(() => <RunScreen id={FAILING_RUN_ID} />);

    expect(container.querySelector(".loading-block")).toHaveTextContent("loading the run");
    expect(container.querySelector("h1")).toBeNull();

    await settle();
    expect(container.querySelector(".loading-block")).toBeNull();
    expect(container.querySelector("h1")).toBeInTheDocument();
  });

  it("renders an unknown id as the platform's not-found literal", async () => {
    const { container } = await screen("01J9NOTHINGSTOREDHEREXX");

    const panel = container.querySelector(".error-panel") as HTMLElement;
    expect(panel).toHaveAttribute("data-tone", "neutral");
    expect(visible(panel.querySelector(".error-panel-word") as Element)).toBe("not found");
    // the literal the author will grep for, never rewritten
    expect(visible(panel.querySelector(".error-panel-said") as Element)).toBe("run-not-found");
    expect(visible(panel.querySelector(".error-panel-explanation") as Element)).toBe(
      "the platform answered, and holds no run under this id.",
    );
    expect(pair(panel, "id")).toBe("01J9NOTHINGSTOREDHEREXX");
    // nothing was read, so nothing on the screen may claim to be a verdict
    expect(container.querySelector("h1")).toBeNull();
  });

  it("renders a transport failure as the state where nothing is known", async () => {
    const { container } = await screen("network-down");

    const panel = container.querySelector(".error-panel") as HTMLElement;
    expect(panel).toHaveAttribute("data-tone", "fail");
    expect(visible(panel.querySelector(".error-panel-word") as Element)).toBe("no response");
    expect(visible(panel.querySelector(".error-panel-said") as Element)).toBe(
      "authoring request failed before receiving an HTTP response",
    );
    expect(container.querySelector("h1")).toBeNull();
  });
});
