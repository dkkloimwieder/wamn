import { describe, expect, it } from "vitest";

import {
  effectUncertainRun,
  failingRun,
  finalizedReport,
  passingRun,
  terminalizedRun,
} from "./fixtures";
import {
  assertableFailureKind,
  durableRunStatus,
  wireFailureKind,
  wireRunStatus,
} from "./run-vocabulary";
import type {
  FlowFailureKind,
  Report,
  Run,
  RunFailureKind,
  RunStatus,
  RunTerminalStatus,
} from "./types";

/**
 * The seam this module exists for: `finalizedReport` asserts
 * `run-terminal-outcome: completed` and `passingRun` reads back `succeeded`.
 * Both words are pinned here against the fixtures, so a rename on either side
 * fails a test rather than quietly showing an author two states.
 */

/**
 * `never` while a list below covers its whole union. A word added to either
 * vocabulary makes the argument that word, which fails this constraint and names
 * itself in the error — so the round trips cannot silently stop covering it.
 */
type Covered<Missing extends never> = Missing;

describe("wireRunStatus", () => {
  it("maps every durable terminal status that has a wire word", () => {
    expect(wireRunStatus("completed")).toEqual({ form: "wire", status: "succeeded" });
    expect(wireRunStatus("failed")).toEqual({ form: "wire", status: "failed" });
    expect(wireRunStatus("effect-uncertain")).toEqual({
      form: "wire",
      status: "effect-uncertain",
    });
  });

  it("answers the absence for infrastructure-failure rather than calling it failed", () => {
    expect(wireRunStatus("infrastructure-failure")).toEqual({ form: "no-wire-form" });
  });
});

describe("durableRunStatus", () => {
  it("maps every wire status that names a terminal outcome", () => {
    expect(durableRunStatus("succeeded")).toEqual({ form: "durable", status: "completed" });
    expect(durableRunStatus("failed")).toEqual({ form: "durable", status: "failed" });
    expect(durableRunStatus("effect-uncertain")).toEqual({
      form: "durable",
      status: "effect-uncertain",
    });
  });

  it("answers not-terminal for the three wire words no assertion can name", () => {
    expect(durableRunStatus("queued")).toEqual({ form: "not-terminal" });
    expect(durableRunStatus("dispatched")).toEqual({ form: "not-terminal" });
    expect(durableRunStatus("running")).toEqual({ form: "not-terminal" });
  });
});

describe("a word neither vocabulary knows", () => {
  it("is named rather than answered with a prototype member", () => {
    expect(durableRunStatus("toString" as RunStatus)).toEqual({ form: "unknown-word" });
    expect(wireRunStatus("constructor" as RunTerminalStatus)).toEqual({ form: "unknown-word" });
    expect(assertableFailureKind("toString" as RunFailureKind)).toEqual({ form: "unknown-word" });
    expect(wireFailureKind("constructor" as FlowFailureKind)).toEqual({ form: "unknown-word" });
  });
});

describe("run status round trip", () => {
  const durableStatuses = [
    "completed",
    "failed",
    "infrastructure-failure",
    "effect-uncertain",
  ] as const satisfies readonly RunTerminalStatus[];
  type _DurableStatusesCovered = Covered<
    Exclude<RunTerminalStatus, (typeof durableStatuses)[number]>
  >;

  const wireStatuses = [
    "queued",
    "dispatched",
    "running",
    "succeeded",
    "failed",
    "effect-uncertain",
  ] as const satisfies readonly RunStatus[];
  type _WireStatusesCovered = Covered<Exclude<RunStatus, (typeof wireStatuses)[number]>>;

  it("returns every durable terminal status that has a wire word", () => {
    for (const status of durableStatuses) {
      const wire = wireRunStatus(status);
      if (wire.form !== "wire") {
        expect(wire).toEqual({ form: "no-wire-form" });
        expect(status).toBe("infrastructure-failure");
        continue;
      }
      expect(durableRunStatus(wire.status)).toEqual({ form: "durable", status });
    }
  });

  it("returns every wire status that names a terminal outcome", () => {
    for (const status of wireStatuses) {
      const durable = durableRunStatus(status);
      if (durable.form !== "durable") {
        expect(durable).toEqual({ form: "not-terminal" });
        continue;
      }
      expect(wireRunStatus(durable.status)).toEqual({ form: "wire", status });
    }
  });
});

describe("wireFailureKind", () => {
  it("maps the four assertable kinds the wire keeps", () => {
    expect(wireFailureKind("terminal")).toEqual({ form: "wire", kind: "terminal" });
    expect(wireFailureKind("retry-exhausted")).toEqual({ form: "wire", kind: "retry-exhausted" });
    expect(wireFailureKind("invalid-input")).toEqual({ form: "wire", kind: "invalid-input" });
    expect(wireFailureKind("effect-uncertain")).toEqual({ form: "wire", kind: "effect-uncertain" });
  });

  it("answers the absence for the three budget kinds", () => {
    expect(wireFailureKind("runaway-budget")).toEqual({ form: "no-wire-form" });
    expect(wireFailureKind("depth-budget")).toEqual({ form: "no-wire-form" });
    expect(wireFailureKind("dispatch-budget")).toEqual({ form: "no-wire-form" });
  });
});

describe("assertableFailureKind", () => {
  it("maps every wire kind an assertion can name", () => {
    expect(assertableFailureKind("invalid-input")).toEqual({
      form: "assertable",
      kind: "invalid-input",
    });
    expect(assertableFailureKind("retry-exhausted")).toEqual({
      form: "assertable",
      kind: "retry-exhausted",
    });
    expect(assertableFailureKind("effect-uncertain")).toEqual({
      form: "assertable",
      kind: "effect-uncertain",
    });
    expect(assertableFailureKind("terminal")).toEqual({ form: "assertable", kind: "terminal" });
  });

  it("answers not-assertable for deadline-exhausted, which no run carries durably", () => {
    expect(assertableFailureKind("deadline-exhausted")).toEqual({ form: "not-assertable" });
  });
});

describe("failure kind round trip", () => {
  const assertableKinds = [
    "terminal",
    "retry-exhausted",
    "invalid-input",
    "runaway-budget",
    "effect-uncertain",
    "depth-budget",
    "dispatch-budget",
  ] as const satisfies readonly FlowFailureKind[];
  type _AssertableKindsCovered = Covered<
    Exclude<FlowFailureKind, (typeof assertableKinds)[number]>
  >;

  const wireKinds = [
    "invalid-input",
    "retry-exhausted",
    "deadline-exhausted",
    "effect-uncertain",
    "terminal",
  ] as const satisfies readonly RunFailureKind[];
  type _WireKindsCovered = Covered<Exclude<RunFailureKind, (typeof wireKinds)[number]>>;

  it("returns every assertable kind that has a wire word", () => {
    for (const kind of assertableKinds) {
      const wire = wireFailureKind(kind);
      if (wire.form !== "wire") {
        expect(wire).toEqual({ form: "no-wire-form" });
        expect(["runaway-budget", "depth-budget", "dispatch-budget"]).toContain(kind);
        continue;
      }
      expect(assertableFailureKind(wire.kind)).toEqual({ form: "assertable", kind });
    }
  });

  it("returns every wire kind an assertion can name", () => {
    for (const kind of wireKinds) {
      const assertable = assertableFailureKind(kind);
      if (assertable.form !== "assertable") {
        expect(assertable).toEqual({ form: "not-assertable" });
        expect(kind).toBe("deadline-exhausted");
        continue;
      }
      expect(wireFailureKind(assertable.kind)).toEqual({ form: "wire", kind });
    }
  });
});

function assertedOutcome(report: Report): RunTerminalStatus {
  const expected =
    report.state === "finalized"
      ? report.cases
          .flatMap((reportCase) => reportCase.failedAssertions)
          .map((failure) => failure.expected)
          .find((assertion) => assertion.family === "run-terminal-outcome")
      : undefined;
  if (expected?.family !== "run-terminal-outcome") {
    throw new Error("the finalized report should carry a run-terminal-outcome assertion");
  }
  return expected.status;
}

function assertedKind(report: Report): FlowFailureKind {
  const expected =
    report.state === "finalized"
      ? report.cases
          .flatMap((reportCase) => reportCase.failedAssertions)
          .map((failure) => failure.expected)
          .find((assertion) => assertion.family === "typed-flow-failure")
      : undefined;
  if (expected?.family !== "typed-flow-failure") {
    throw new Error("the finalized report should carry a typed-flow-failure assertion");
  }
  return expected.kind;
}

function failureKind(run: Run): RunFailureKind {
  if (run.failure === null) {
    throw new Error(`the ${run.runId} fixture should carry a failure`);
  }
  return run.failure.kind;
}

describe("against the fixtures", () => {
  it("reads the report's `completed` and a passing run's `succeeded` as one state", () => {
    expect(assertedOutcome(finalizedReport)).toBe("completed");
    expect(wireRunStatus(assertedOutcome(finalizedReport))).toEqual({
      form: "wire",
      status: passingRun.status,
    });
  });

  it("reads the failing run's wire kind back as the kind its case asserted", () => {
    expect(wireFailureKind(assertedKind(finalizedReport))).toEqual({
      form: "wire",
      kind: failureKind(failingRun),
    });
  });

  it("reads an unresolved effect-uncertain run in both vocabularies", () => {
    expect(durableRunStatus(effectUncertainRun.status)).toEqual({
      form: "durable",
      status: "effect-uncertain",
    });
    expect(assertableFailureKind(failureKind(effectUncertainRun))).toEqual({
      form: "assertable",
      kind: "effect-uncertain",
    });
  });

  it("reads a terminalized run as durable failed with an effect-uncertain kind", () => {
    expect(durableRunStatus(terminalizedRun.status)).toEqual({ form: "durable", status: "failed" });
    expect(assertableFailureKind(failureKind(terminalizedRun))).toEqual({
      form: "assertable",
      kind: "effect-uncertain",
    });
  });
});
