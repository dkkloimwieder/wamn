import { cleanup, render } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";

import {
  captureOffRun,
  effectUncertainRun,
  failingRun,
  passingRun,
  terminalizedRun,
  truncatedRun,
} from "../reader/fixtures";
import type { Run, RunFailureKind, RunStatus } from "../reader/types";
import { runTone } from "../ui/status";
import { RunVerdict, runVerdictLine } from "./run-verdict";

afterEach(cleanup);

const runs: readonly Run[] = [
  passingRun,
  failingRun,
  effectUncertainRun,
  terminalizedRun,
  truncatedRun,
  captureOffRun,
];

const failure = failingRun.failure;
if (failure === null) {
  throw new Error("the failing run fixture must carry a run failure");
}

/**
 * What a failed run looks like once it arrives over the wire rather than out of
 * a fixture: `RunFailure.node` is a `runs` column no wire field carries, so the
 * node is the one part of §2.2's line that is routinely missing. Derived from
 * the wireframe's own run, so the missing node is the only difference.
 */
const nodelessRun: Run = { ...failingRun, failure: { ...failure, node: null } };

/**
 * A draft whose id is not its flow id. Every fixture sets the two the same, so
 * only a locally built run can prove which of them addresses the route and
 * which one the line reads.
 */
const relabelledRun: Run = {
  ...failingRun,
  draft: { draftId: "01J9RD8Q4M0S3F2K", flowId: "orders", revision: 17 },
};

describe("runVerdictLine", () => {
  it("reads §2.2's wireframe off the run the wireframe was drawn from", () => {
    expect(runVerdictLine(failingRun)).toBe("FAILED · retry-exhausted at fetch-inventory");
  });

  it("states the status alone when the run did not fail", () => {
    expect(runVerdictLine(passingRun)).toBe("SUCCEEDED");
    expect(runVerdictLine(truncatedRun)).toBe("SUCCEEDED");
    expect(runVerdictLine(captureOffRun)).toBe("SUCCEEDED");
  });

  it("says the uncertain status once, not twice", () => {
    // the status word and the fail kind are the same word on this run
    expect(runVerdictLine(effectUncertainRun)).toBe("EFFECT-UNCERTAIN at charge-card");
  });

  it("never lets a terminalized run read as an ordinary failure", () => {
    // status `failed`, kind still `effect-uncertain` — the kind is the whole
    // difference, so it is the one thing the line cannot drop
    expect(runVerdictLine(terminalizedRun)).toBe("FAILED · effect-uncertain at charge-card");
  });

  it("drops the failing node rather than printing that it is missing", () => {
    expect(runVerdictLine(nodelessRun)).toBe("FAILED · retry-exhausted");
    expect(runVerdictLine(nodelessRun)).not.toContain("null");
  });

  it("states the bare status word before a run reaches an outcome", () => {
    // no fixture carries these three: a run only becomes worth reading once it
    // has an outcome, but the line has to be true of the whole vocabulary
    for (const status of ["queued", "dispatched", "running"] as const) {
      expect(runVerdictLine({ ...passingRun, status })).toBe(status.toUpperCase());
    }
  });

  it("names every fail kind the wire admits", () => {
    for (const kind of [
      "invalid-input",
      "retry-exhausted",
      "deadline-exhausted",
      "terminal",
    ] as const) {
      expect(runVerdictLine({ ...failingRun, failure: { ...failure, kind } })).toBe(
        `FAILED · ${kind} at fetch-inventory`,
      );
    }
  });

  it("overlaps the status and fail-kind vocabularies in exactly one word", () => {
    // the premise of the line's one suppression rule. `run-vocabulary.ts`
    // (wamn-dggp.14) translates both vocabularies; if that ever makes a second
    // word coincide, the line would start dropping a kind that matters, so the
    // change has to land here rather than on the screen. The `satisfies` keeps
    // both lists exhaustive.
    const statuses: string[] = Object.keys({
      queued: null,
      dispatched: null,
      running: null,
      succeeded: null,
      failed: null,
      "effect-uncertain": null,
    } satisfies Record<RunStatus, null>);
    const kinds: string[] = Object.keys({
      "invalid-input": null,
      "retry-exhausted": null,
      "deadline-exhausted": null,
      "effect-uncertain": null,
      terminal: null,
    } satisfies Record<RunFailureKind, null>);

    expect(kinds.filter((kind) => statuses.includes(kind))).toEqual(["effect-uncertain"]);
  });
});

describe("RunVerdict", () => {
  it("takes its tone from the run status and its heading from the verdict line", () => {
    expect(new Set(runs.map((run) => runTone(run.status)))).toEqual(
      new Set(["ok", "fail", "uncertain"]),
    );

    for (const run of runs) {
      const { container } = render(() => <RunVerdict run={run} />);
      expect(container.querySelector(".verdict-bar")).toHaveAttribute(
        "data-tone",
        runTone(run.status),
      );
      expect(container.querySelector("h1")).toHaveTextContent(runVerdictLine(run));
      cleanup();
    }
  });

  it("renders §2.2's identity line: run, draft, trace, capture", () => {
    const { container, getByRole } = render(() => <RunVerdict run={failingRun} />);

    // every id is a copy affordance (§2.6), and each says which id it copies
    getByRole("button", { name: `copy run id ${failingRun.runId}` });
    getByRole("button", { name: `copy trace id ${failingRun.traceId}` });

    const draft = getByRole("link", {
      name: `draft ${failingRun.draft.flowId}@${failingRun.draft.revision}`,
    });
    // §2.4's route, built by `toHash` — the arrow is decoration and unreachable
    expect(draft).toHaveAttribute("href", "#/draft/orders/17");

    expect(container.querySelector(".run-identity")).toHaveTextContent("captured full");
  });

  it("states the capture mode in words on a run that captured nothing", () => {
    const { container } = render(() => <RunVerdict run={captureOffRun} />);
    expect(container.querySelector(".run-identity")).toHaveTextContent("captured off");
  });

  it("links the draft by its id while the line reads its flow", () => {
    const { getByRole } = render(() => <RunVerdict run={relabelledRun} />);
    expect(getByRole("link", { name: "draft orders@17" })).toHaveAttribute(
      "href",
      "#/draft/01J9RD8Q4M0S3F2K/17",
    );
  });

  it("renders a run that has reached no outcome in the neutral tone", () => {
    const { container } = render(() => <RunVerdict run={{ ...passingRun, status: "running" }} />);
    expect(container.querySelector(".verdict-bar")).toHaveAttribute("data-tone", "neutral");
    expect(container.querySelector("h1")).toHaveTextContent("RUNNING");
  });
});
