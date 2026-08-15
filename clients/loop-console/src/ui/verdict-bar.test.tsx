import type { JSX } from "@solidjs/web";
import { cleanup, render } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";

import {
  allPassReport,
  effectUncertainRun,
  failingRun,
  parsingDraft,
  pendingReport,
} from "../reader/fixtures";
import { runTone } from "./status";
import type { Tone } from "./status";
import { VerdictBar } from "./verdict-bar";

afterEach(cleanup);

/** One realistic verdict line per tone, each composed from the fixture that produces it. */
function bars(): ReadonlyArray<{ tone: Tone; verdict: string }> {
  const failure = failingRun.failure;
  const uncertain = effectUncertainRun.failure;
  if (
    failure === null ||
    uncertain === null ||
    allPassReport.state !== "finalized" ||
    pendingReport.state !== "pending"
  ) {
    throw new Error("expected both run failures, the all-pass report, and the pending one");
  }
  return [
    {
      // §2.2: the status word, the run-level fail kind, the failing node
      tone: runTone(failingRun.status),
      verdict: `${failingRun.status.toUpperCase()} · ${failure.kind} at ${failure.node}`,
    },
    {
      // §2.3: all-pass renders ok, any-fail fail
      tone: allPassReport.passed === allPassReport.total ? "ok" : "fail",
      verdict: `${allPassReport.passed} / ${allPassReport.total} PASSED`,
    },
    {
      tone: runTone(effectUncertainRun.status),
      verdict: `${effectUncertainRun.status.toUpperCase()} · ${uncertain.node}`,
    },
    {
      // §2.3: pending renders warn — and this report carries no contracted reason
      tone: "warn",
      verdict: pendingReport.pendingReason ?? "PENDING",
    },
    {
      // §2.4: the draft screen's verdict is its identity, in the neutral tone
      tone: "neutral",
      verdict: `DRAFT ${parsingDraft.flowId} @ ${parsingDraft.revision}`,
    },
  ];
}

describe("VerdictBar", () => {
  it("renders all five tones, and names each one on the rule", () => {
    const all = bars();
    expect(new Set(all.map((bar) => bar.tone))).toEqual(
      new Set(["fail", "ok", "uncertain", "warn", "neutral"]),
    );

    for (const bar of all) {
      const { container } = render(() => <VerdictBar tone={bar.tone} verdict={bar.verdict} />);
      expect(container.querySelector(".verdict-bar")).toHaveAttribute("data-tone", bar.tone);
      // the word on the rule is what keeps the bar from stating its state in colour alone
      expect(container.querySelector(".verdict-rule")).toHaveTextContent(bar.tone);
      expect(container.querySelector(".verdict")).toHaveTextContent(bar.verdict);
      cleanup();
    }
  });

  it("makes the verdict the screen's heading", () => {
    const [bar] = bars();
    const { container } = render(() => <VerdictBar tone={bar.tone} verdict={bar.verdict} />);
    expect(container.querySelector("h1")).toHaveTextContent(
      "FAILED · retry-exhausted at fetch-inventory",
    );
  });

  it("renders the identity line only when a screen fills the slot", () => {
    const [bar] = bars();
    // the slot is filled with CopyIds, so building it twice would mount clipboard
    // state the document never receives — it has to be instantiated exactly once
    let built = 0;
    function Identity(): JSX.Element {
      built += 1;
      return <span>run {failingRun.runId}</span>;
    }

    const { container } = render(() => (
      <VerdictBar tone={bar.tone} verdict={bar.verdict} identity={<Identity />} />
    ));
    expect(container.querySelector(".verdict-identity")).toHaveTextContent(
      `run ${failingRun.runId}`,
    );
    expect(built).toBe(1);
    cleanup();

    const bare = render(() => <VerdictBar tone={bar.tone} verdict={bar.verdict} />);
    expect(bare.container.querySelector(".verdict-identity")).toBeNull();
  });
});
