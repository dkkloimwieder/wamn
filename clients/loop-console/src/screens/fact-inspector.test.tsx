import { cleanup, fireEvent, render } from "@solidjs/testing-library";
import { flush } from "solid-js";
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
import { FactInspector } from "./fact-inspector";

/**
 * Every state below is driven by a step-2 run fixture, in the row §2.2 draws it
 * on: the failing run for before/write/after context and a retried node, the
 * passing run for final-only context and a too-large output, the capture-off run
 * for both blank sides, and the two uncertain runs for the one pair the console
 * exists to keep apart.
 */

afterEach(cleanup);

/** The fact this run returned at `#index`, so every case names a wireframe row. */
function factOf(run: Run, index: number): ExecutionFact {
  const fact = run.facts.find((candidate) => candidate.index === index);
  if (fact === undefined) {
    throw new Error(`the fixture returned no fact ${index}`);
  }
  return fact;
}

function inspect(run: Run, index: number, compressed?: boolean): HTMLElement {
  return inspectFact(factOf(run, index), run, compressed);
}

/**
 * A fact the fixtures do not carry, for the states the domain admits and no
 * fixture reaches: a hashless too-large value, a child frame, an errored input.
 */
function inspectFact(fact: ExecutionFact, run: Run, compressed?: boolean): HTMLElement {
  return render(() => <FactInspector fact={fact} run={run} compressed={compressed} />).container;
}

function laneOf(container: HTMLElement, label: string): HTMLElement {
  const lane = [...container.querySelectorAll<HTMLElement>(".fact-lane")].find(
    (candidate) => candidate.querySelector(".fact-lane-label")?.textContent === label,
  );
  if (lane === undefined) {
    throw new Error(`no ${label} lane`);
  }
  return lane;
}

/** One lane's text, so no assertion here can be satisfied by a neighbouring lane. */
function laneText(container: HTMLElement, label: string): string {
  return (laneOf(container, label).textContent ?? "").replace(/\s+/g, " ");
}

/** The tone the STATE lane paints its status word with — §1.1's colour, never alone. */
function stateTone(container: HTMLElement): string | null {
  return laneOf(container, "state").querySelector("[data-tone]")?.getAttribute("data-tone") ?? null;
}

describe("FactInspector", () => {
  it("opens the three lanes §6 step 4 spells out", () => {
    const container = inspect(failingRun, 4);

    const labels = [...container.querySelectorAll(".fact-lane-label")].map(
      (label) => label.textContent,
    );
    expect(labels).toEqual(["data", "context", "state"]);
  });

  // ── data ─────────────────────────────────────────────────────────────────

  it("renders each captured side as its own document, under the branch taken", () => {
    // §2.2's `→ in-stock` row: both sides present, and a port that is not `main`.
    const container = inspect(failingRun, 3);
    const data = laneText(container, "data");

    expect(data).toContain("input");
    expect(data).toContain('"quantity": 2');
    expect(data).toContain("→ in-stock");
    expect(data).toContain('"available": 0');
    // two documents, two views, and one subject each so their copy controls differ
    expect(laneOf(container, "data").querySelectorAll(".json-tree")).toHaveLength(2);
  });

  it("reads §2.2's words on both sides when capture was off", () => {
    const container = inspect(captureOffRun, 1);
    const data = laneText(container, "data");

    expect(data.match(/capture was off for this run/g)).toHaveLength(2);
    expect(laneOf(container, "data").querySelector(".json-tree")).toBeNull();
  });

  it("states the size and hash of an output too large to capture, and no value", () => {
    // `reserve` cleared the write-side ceiling: the row kept a size and a hash.
    const container = inspect(passingRun, 3);
    const data = laneText(container, "data");

    expect(data).toContain("too large to capture — 98,304 bytes, hash 9f31c0a74be2d586");
    // the input is still present, so exactly one document renders — not the output
    expect(laneOf(container, "data").querySelectorAll(".json-tree")).toHaveLength(1);
  });

  it("says a too-large output kept no hash rather than printing an empty one", () => {
    // `hash` is nullable on the state, so the row can keep a size and nothing else.
    const fact = factOf(passingRun, 3);
    const container = inspectFact(
      { ...fact, output: { state: "too-large", size: 98_304, hash: null } },
      passingRun,
    );
    const data = laneText(container, "data");

    expect(data).toContain("too large to capture — 98,304 bytes, no hash recorded");
    expect(data).not.toContain("hash null");
    expect(laneOf(container, "data").querySelectorAll(".json-tree")).toHaveLength(1);
  });

  it("keeps an outstanding outcome distinct from a failed one", () => {
    // The same node of the same charge flow, before and after terminalization.
    const outstanding = laneText(inspect(effectUncertainRun, 2), "data");
    cleanup();
    const errored = laneText(inspect(terminalizedRun, 2), "data");

    expect(outstanding).toContain("the outcome is unknown");
    // rendering an outstanding effect as errored asserts exactly the wrong conclusion
    expect(outstanding).not.toContain("failed");
    expect(errored).toContain("the node failed, so there is no output");
    expect(errored).not.toContain("unknown");
  });

  it("names the slot an absence stands in, so the input never reads as the output", () => {
    // The reader produces `errored` only for the output today; if it ever does not,
    // the sentence has to be about the side it is printed on.
    const fact = factOf(failingRun, 4);
    const container = inspectFact({ ...fact, input: { state: "errored" } }, failingRun);
    const data = laneText(container, "data");

    expect(data).toContain("the node failed, so there is no input");
    expect(data).toContain("the node failed, so there is no output");
  });

  it("says a node emitted no port rather than assuming main", () => {
    const container = inspect(effectUncertainRun, 2);

    expect(laneText(container, "data")).toContain("no port emitted");
  });

  // ── context ──────────────────────────────────────────────────────────────

  it("shows before, write and after, and says a write replaces the whole document", () => {
    // The one context write of the failing run.
    const container = inspect(failingRun, 2);
    const context = laneText(container, "context");

    expect(context).toContain("before");
    expect(context).toContain("write");
    expect(context).toContain("after");
    expect(context).toContain("replaces the whole document — a write is never a merge");
    expect(context).toContain('"sku": "SKU-118"');
  });

  it("says the node left context unchanged rather than drawing an empty write", () => {
    const container = inspect(failingRun, 3);
    const context = laneText(container, "context");

    expect(context).toContain("this node left the context unchanged");
    // before and after are still documents; only the write is a sentence
    expect(laneOf(container, "context").querySelectorAll(".json-tree")).toHaveLength(2);
  });

  it("renders the final-only cell as the run's, claiming nothing for this node", () => {
    const container = inspect(passingRun, 1);
    const context = laneText(container, "context");

    expect(context).toContain("the run's final context");
    expect(context).toContain("says nothing about what this node did");
    expect(context).toContain('"order": "ord-4471"');
    expect(laneOf(container, "context").querySelectorAll(".json-tree")).toHaveLength(1);
  });

  it("says an absent context was never captured, not that it was empty", () => {
    const container = inspect(captureOffRun, 1);
    const context = laneText(container, "context");

    expect(context).toContain("this run captured no context");
    expect(context).not.toContain("empty");
  });

  // ── state ────────────────────────────────────────────────────────────────

  it("renders the recorded state of the retried visit", () => {
    const fact = factOf(failingRun, 4);
    const { container, getByRole } = render(() => (
      <FactInspector fact={fact} run={failingRun} />
    ));
    const state = laneText(container, "state");

    expect(state).toContain("error");
    // §1.1: the word carries the status and the tone reinforces it — never the reverse
    expect(stateTone(container)).toBe("fail");
    expect(state).toContain("retryable · upstream 503");
    // §2.2's `retryable ×4` is attempt + 1 — four attempts of one visit
    expect(state).toContain("×4 of this visit");
    expect(state).toContain("root");
    expect(state).toContain("1,204 ms");
    // the hash is long, and named by its fact: one run's facts share it
    expect(getByRole("button", { name: `copy fact 4 plan hash id ${fact.planHash}` })).toBeTruthy();
  });

  it("reads two facts for one node as two visits, never as a retry", () => {
    // A retry never produces a second fact — the primary key collapses it — so
    // these two rows are visit 1 and visit 2 of `fetch-inventory`, and the four
    // attempts of the first visit live on its `attempt` alone.
    expect(factOf(failingRun, 4).node).toBe(factOf(failingRun, 5).node);

    const first = laneText(inspect(failingRun, 4), "state");
    cleanup();
    const second = laneText(inspect(failingRun, 5), "state");

    expect(first).toContain("visit 1 of 2 returned for this node");
    expect(first).toContain("×4 of this visit");
    expect(second).toContain("visit 2 of 2 returned for this node");
    expect(second).toContain("×1 of this visit");
    expect(second).not.toContain("retry");
  });

  it("counts the visits this read returned, not the visits the run made", () => {
    // A truncated read shows fewer visits than the run made, which is why the
    // sentence says `returned` and never claims to count the run.
    expect(truncatedRun.factCount.total).toBe(3412);
    const state = laneText(inspect(truncatedRun, 1), "state");

    expect(state).toContain("visit 1 of 200 returned for this node");
    expect(state).not.toContain("3,412");
  });

  it("names the parent frame rather than reading a falsy one as the root", () => {
    // Frame 0 *is* the root, so a truthiness test would print `root` for this child.
    const fact = factOf(failingRun, 3);
    const container = inspectFact({ ...fact, frameId: 1, parentFrameId: 0 }, failingRun);
    const state = laneText(container, "state");

    expect(state).toContain("parent 0");
    expect(state).not.toContain("root");
  });

  it("spells out the attempt and duration the platform never recorded", () => {
    // Nothing durable was written for this visit, so neither survives.
    const container = inspect(effectUncertainRun, 2);
    const state = laneText(container, "state");

    expect(state).toContain("started");
    expect(stateTone(container)).toBe("neutral");
    // named pairs, not a phrase count: `attempt 0` is a first execution, not an absence
    expect(state).toContain("attemptnot recorded");
    expect(state).toContain("durationnot recorded");
    expect(state).not.toContain("of this visit");
    expect(state).not.toContain("ms");
  });

  it("makes no claim about whether a node is effectful", () => {
    // `parse-order` is a transform — the node type a `— pure node` guess would
    // read off — and the read carries no effectfulness marker to justify one.
    const container = inspect(passingRun, 2);
    const state = laneText(container, "state");

    expect(state).toContain("no effect attempts");
    expect(state).not.toContain("pure node");
    expect(stateTone(container)).toBe("ok");
  });

  // ── trace mode ───────────────────────────────────────────────────────────

  it("compresses a document to two lines behind a [json] pop", () => {
    const container = inspect(failingRun, 2, true);
    const data = laneOf(container, "data");

    const preview = data.querySelector(".fact-json-preview");
    expect((preview?.textContent ?? "").split("\n")).toHaveLength(2);
    expect(preview).toHaveTextContent("…");
    expect(data.querySelector(".json-tree")).toBeNull();

    const [pop] = [...data.querySelectorAll("button")].filter((button) =>
      (button.textContent ?? "").includes("[json] fact 2 input"),
    );
    fireEvent.click(pop);
    flush();

    expect(data.querySelector(".json-tree")).toHaveTextContent('"quantity": 2');
  });

  it("says after unchanged rather than reprinting a document the node did not write", () => {
    const compressed = laneText(inspect(failingRun, 3, true), "context");
    cleanup();
    const whole = laneText(inspect(failingRun, 3), "context");

    expect(compressed).toContain("after unchanged");
    // the full inspector still shows the document, so nothing is hidden by default
    expect(whole).not.toContain("after unchanged");
  });

  it("shows an after document that differs rather than claiming it did not", () => {
    // A read whose `write: null` disagrees with its own `after`. Trace mode is the
    // one place a wrong claim is not visibly checkable, so the claim is checked.
    const fact = factOf(failingRun, 3);
    const context = laneText(
      inspectFact(
        {
          ...fact,
          context: { mode: "before-write-after", before: {}, write: null, after: { order: 1 } },
        },
        failingRun,
        true,
      ),
      "context",
    );

    expect(context).not.toContain("after unchanged");
    expect(context).toContain('"order": 1');
  });

  it("names every control by the fact and the side it acts on", () => {
    // §3's floor at the density this screen reaches: five documents can open in
    // one row, and two controls that act on different values may not share a name.
    // Two facts of one run at once, because that is where names collide: they
    // share a plan hash, and every document has a counterpart on the other fact.
    const { container } = render(() => (
      <>
        <FactInspector fact={factOf(failingRun, 4)} run={failingRun} compressed />
        <FactInspector fact={factOf(failingRun, 5)} run={failingRun} compressed />
      </>
    ));
    for (let pass = 0; pass < 8; pass += 1) {
      const closed = [...container.querySelectorAll<HTMLElement>('button[aria-expanded="false"]')];
      if (closed.length === 0) {
        break;
      }
      for (const button of closed) {
        fireEvent.click(button);
      }
      flush();
    }

    const names = [...container.querySelectorAll("button")].map((button) =>
      (button.getAttribute("aria-label") ?? button.textContent ?? "").replace(/[▸▾\s]+/g, " ").trim(),
    );
    // the scan is worth nothing if it never sees the controls it is scanning
    expect(names.length).toBeGreaterThan(4);
    expect(new Set(names).size).toBe(names.length);
  });
});
