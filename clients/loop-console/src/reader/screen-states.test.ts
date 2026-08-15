import { describe, expect, it } from "vitest";

import type { ReadResult } from "./errors";
import { fixtureReader } from "./fixture-reader";
import {
  ALL_PASS_REPORT_ID,
  CAPTURE_OFF_RUN_ID,
  EFFECT_UNCERTAIN_RUN_ID,
  FAILING_RUN_ID,
  FINALIZED_REPORT_ID,
  PASSING_RUN_ID,
  PENDING_REPORT_ID,
  TERMINALIZED_RUN_ID,
  TRUNCATED_RUN_ID,
} from "./fixtures";
import { EFFECT_UNCERTAIN_MEANING, type AssertionFamily, type Report, type Run } from "./types";

/**
 * The done-check's second clause, enumerated: one assertion per screen state
 * every later step has to render, each read through the reader by its id.
 */

function value<T>(result: ReadResult<T>): T {
  if (!result.ok) {
    throw new Error(`expected a value, got ${result.error.kind}`);
  }
  return result.value;
}

const run = async (id: string): Promise<Run> => value(await fixtureReader.runs.get(id));
const report = async (id: string): Promise<Report> => value(await fixtureReader.reports.get(id));

describe("run screen states", () => {
  it("passing run", async () => {
    const passing = await run(PASSING_RUN_ID);
    expect(passing.status).toBe("succeeded");
    expect(passing.failure).toBeNull();
  });

  it("failing run, with its fail kind and failing node", async () => {
    const failing = await run(FAILING_RUN_ID);
    expect(failing.status).toBe("failed");
    expect(failing.failure).toMatchObject({
      kind: "retry-exhausted",
      node: "fetch-inventory",
      detail: "upstream 503 after 4 attempts",
    });
  });

  it("taken branch", async () => {
    const facts = (await run(FAILING_RUN_ID)).facts;
    expect(facts.find((fact) => fact.node === "check-stock")?.outputPort).toBe("in-stock");
  });

  it("context write", async () => {
    const facts = (await run(FAILING_RUN_ID)).facts;
    expect(facts.find((fact) => fact.node === "parse-order")?.context).toEqual({
      mode: "before-write-after",
      before: {},
      write: { order: { sku: "SKU-118", quantity: 2 } },
      after: { order: { sku: "SKU-118", quantity: 2 } },
    });
  });

  it("a whole-run last-wins fold, including the nodes that write nothing", async () => {
    const facts = (await run(FAILING_RUN_ID)).facts;
    // The mode is a property of the run: one fact in another mode makes the
    // fold unfoldable, so every fact carries it.
    const lanes = facts.map((fact) => fact.context);
    expect(lanes.every((lane) => lane.mode === "before-write-after")).toBe(true);

    // `write: null` — the node left context unchanged — is the state of every
    // fact but one, and each fact's `before` is the previous fact's `after`.
    let carried: unknown = {};
    for (const lane of lanes) {
      if (lane.mode !== "before-write-after") {
        throw new Error("expected the before-write-after mode");
      }
      expect(lane.before).toEqual(carried);
      expect(lane.after).toEqual(lane.write === null ? lane.before : lane.write);
      carried = lane.after;
    }
    expect(lanes.filter((lane) => lane.mode === "before-write-after" && lane.write === null))
      .toHaveLength(4);
    expect(carried).toEqual({ order: { sku: "SKU-118", quantity: 2 } });
  });

  it("retried effectful node, as two occurrences of one node", async () => {
    const facts = (await run(FAILING_RUN_ID)).facts;
    const visits = facts.filter((fact) => fact.node === "fetch-inventory");
    // Two facts means two visits; the four retries live on the first visit's attempt.
    expect(visits.map((fact) => [fact.occurrence, fact.attempt, fact.failure?.kind])).toEqual([
      [0, 3, "retryable"],
      [1, 0, "terminal"],
    ]);
  });

  it("the other two context lane modes, each on a whole run", async () => {
    const captureOff = await run(CAPTURE_OFF_RUN_ID);
    const passing = await run(PASSING_RUN_ID);
    expect(captureOff.facts.every((fact) => fact.context.mode === "absent")).toBe(true);
    expect(passing.facts.every((fact) => fact.context.mode === "final-only")).toBe(true);
    expect(passing.facts[0].context).toEqual({
      mode: "final-only",
      final: { order: "ord-4471", reserved: true },
    });
  });

  it("the outstanding effect is not reported as errored", async () => {
    const uncertain = await run(EFFECT_UNCERTAIN_RUN_ID);
    const outstanding = uncertain.facts[1];
    // Started, not failed: the console must not claim an outcome it does not have.
    expect(outstanding.status).toBe("started");
    expect(outstanding.output).toEqual({ state: "outstanding" });
    // Nothing durable was written, so not even the attempt number survives.
    expect(outstanding.attempt).toBeNull();

    // Terminalizing it is what turns the same row into a failed one.
    const resolved = await run(TERMINALIZED_RUN_ID);
    expect(resolved.facts[1].status).toBe("error");
    expect(resolved.facts[1].output).toEqual({ state: "errored" });
  });

  it("effect-uncertain, unresolved, with the platform meaning verbatim", async () => {
    const uncertain = await run(EFFECT_UNCERTAIN_RUN_ID);
    expect(uncertain.status).toBe("effect-uncertain");
    expect(uncertain.uncertainty).toEqual({
      meaning: EFFECT_UNCERTAIN_MEANING,
      resolution: { state: "unresolved" },
    });
    expect(EFFECT_UNCERTAIN_MEANING[0]).toBe(
      "An effectful attempt may have escaped without a durable outcome, so the platform cannot safely dispatch it again or claim whether it occurred.",
    );
  });

  it("effect-uncertain, operator-terminalized, keeping the resolved triple", async () => {
    const resolved = await run(TERMINALIZED_RUN_ID);
    expect(resolved.status).toBe("failed");
    expect(resolved.failure?.kind).toBe("effect-uncertain");
    expect(resolved.uncertainty?.resolution).toMatchObject({
      state: "operator-terminalized",
      basis: "counterparty-confirmation",
      terminalReason: "operator-terminalized-effect-uncertain",
    });
  });

  it("truncated, with counts that match the returned facts", async () => {
    const truncated = await run(TRUNCATED_RUN_ID);
    expect(truncated.factCount).toEqual({ returned: 200, total: 3412, truncated: true });
    expect(truncated.facts).toHaveLength(truncated.factCount.returned);
  });

  it("untruncated, so the header can say `all`", async () => {
    const passing = await run(PASSING_RUN_ID);
    expect(passing.factCount).toEqual({ returned: 4, total: 4, truncated: false });
  });

  it("capture off, on both input and output", async () => {
    const captureOff = await run(CAPTURE_OFF_RUN_ID);
    expect(captureOff.capture).toBe("off");
    for (const fact of captureOff.facts) {
      expect(fact.input.state).toBe("capture-off");
      expect(fact.output.state).toBe("capture-off");
    }
  });

  it("the other three captured-output states", async () => {
    const passing = await run(PASSING_RUN_ID);
    const failing = await run(FAILING_RUN_ID);
    expect(passing.facts[0].output).toEqual({
      state: "present",
      value: { order: { sku: "SKU-118", quantity: 2 } },
    });
    expect(passing.facts[2].output).toEqual({
      state: "too-large",
      size: 98_304,
      hash: "9f31c0a74be2d586",
    });
    expect(failing.facts[3].output).toEqual({ state: "errored" });
  });

  it("every run executes nodes its own draft defines", async () => {
    const everyRun = [
      PASSING_RUN_ID,
      FAILING_RUN_ID,
      EFFECT_UNCERTAIN_RUN_ID,
      TERMINALIZED_RUN_ID,
      TRUNCATED_RUN_ID,
      CAPTURE_OFF_RUN_ID,
    ];
    for (const id of everyRun) {
      const executed = await run(id);
      const source = value(
        await fixtureReader.drafts.get(executed.draft.draftId, executed.draft.revision),
      );
      const defined = new Set(
        [...source.definition.matchAll(/"id": "([^"]+)"/g)].map((match) => match[1]),
      );
      // The verdict bar links `draft orders@17 →`; the link has to lead to the
      // source the run's nodes are in, and `nodeType` is joined from it by id.
      for (const fact of executed.facts) {
        expect(defined).toContain(fact.node);
      }
    }
  });
});

describe("report screen states", () => {
  it("finalized 11 of 12", async () => {
    const finalized = await report(FINALIZED_REPORT_ID);
    expect(finalized.state).toBe("finalized");
    if (finalized.state !== "finalized") {
      return;
    }
    expect([finalized.passed, finalized.total]).toEqual([11, 12]);
    expect(finalized.cases).toHaveLength(12);
    expect(finalized.cases.filter((entry) => entry.passed)).toHaveLength(11);
  });

  it("finalized 12 of 12, the verdict bar's `ok` rendering", async () => {
    const finalized = await report(ALL_PASS_REPORT_ID);
    if (finalized.state !== "finalized") {
      throw new Error("expected a finalized report");
    }
    // §2.3: all-pass renders `ok`, any-fail `fail`. The verdict is `passed === total`.
    expect(finalized.passed).toBe(finalized.total);
    expect([finalized.passed, finalized.total]).toEqual([12, 12]);
    expect(finalized.cases).toHaveLength(12);
    expect(finalized.cases.every((entry) => entry.passed)).toBe(true);
    expect(finalized.cases.every((entry) => entry.failedAssertions.length === 0)).toBe(true);
  });

  it("a passing case with no run to hand off to", async () => {
    const finalized = await report(ALL_PASS_REPORT_ID);
    if (finalized.state !== "finalized") {
      throw new Error("expected a finalized report");
    }
    // No wire field exposes the case→run link, so §2.3's `run →` must degrade.
    expect(finalized.cases.filter((entry) => entry.runId === null)).toHaveLength(1);
  });

  it("pending, with no contracted reason to render", async () => {
    const pending = await report(PENDING_REPORT_ID);
    expect(pending.state).toBe("pending");
    if (pending.state !== "pending") {
      return;
    }
    // The pending projection is three fields and none of them is a reason.
    expect(pending.pendingReason).toBeNull();
  });

  it("failed case, one expected/observed pair per assertion family", async () => {
    const finalized = await report(FINALIZED_REPORT_ID);
    if (finalized.state !== "finalized") {
      throw new Error("expected a finalized report");
    }
    const failed = finalized.cases.find((entry) => !entry.passed);
    expect(failed).toMatchObject({
      caseId: "backorders-when-out-of-stock",
      failureKind: "assertion-failed",
      runId: FAILING_RUN_ID,
    });

    const families: AssertionFamily[] = [
      "named-node-terminal",
      "run-terminal-outcome",
      "typed-flow-failure",
      "terminal-respond",
    ];
    expect(failed?.failedAssertions.map((entry) => entry.expected.family)).toEqual(families);
    for (const entry of failed?.failedAssertions ?? []) {
      expect(entry.observed.length).toBeGreaterThan(0);
    }

    const selector = failed?.failedAssertions[0].expected;
    expect(selector).toEqual({
      family: "named-node-terminal",
      flowId: "orders",
      nodeId: "check-stock",
      status: "error",
      failureKind: "terminal",
    });
    // §2.3 renders terminal-respond bodies as JSON, so the body must be a value.
    const respond = failed?.failedAssertions[3].expected;
    expect(respond).toMatchObject({ family: "terminal-respond", status: 201 });
  });
});

describe("draft screen states", () => {
  it("parses, and reports its own true byte length", async () => {
    const draft = value(await fixtureReader.drafts.get("orders", 17));
    expect(draft.parse).toEqual({ ok: true });
    expect(draft.byteLength).toBe(new TextEncoder().encode(draft.definition).length);
    expect(draft.definition.startsWith('{\n  "nodes": [\n')).toBe(true);
  });

  it("does not parse, and says which line", async () => {
    const draft = value(await fixtureReader.drafts.get("orders", 16));
    expect(draft.parse).toEqual({
      ok: false,
      line: 41,
      message: "Expected ':' after property name",
    });
  });

  it("neither saved-at nor provenance, which is what a real read returns", async () => {
    const unattributed = value(await fixtureReader.drafts.get("charges", 3));
    expect(unattributed.savedAt).toBeNull();
    expect(unattributed.provenance).toBeNull();
    // The bytes are still there — those are the only two fields the wire gives.
    expect(unattributed.parse).toEqual({ ok: true });
    expect(unattributed.byteLength).toBeGreaterThan(0);
  });

  it("dirty provenance", async () => {
    const dirty = value(await fixtureReader.drafts.get("orders", 16));
    const clean = value(await fixtureReader.drafts.get("orders", 17));
    expect(dirty.provenance).toEqual({
      commit: "2d84b06fe7a1359c48db20f6ac57193ed80b4a2f",
      ref: null,
      dirty: true,
    });
    expect(clean.provenance?.dirty).toBe(false);
  });
});
