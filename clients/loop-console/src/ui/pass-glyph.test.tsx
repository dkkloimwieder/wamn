import { cleanup, render } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";

import { finalizedReport } from "../reader/fixtures";
import type { ReportCase } from "../reader/types";
import { PassGlyph } from "./pass-glyph";

afterEach(cleanup);

/** The fixture is typed as the report union; the cases live in the finalized arm. */
function cases(): readonly ReportCase[] {
  if (finalizedReport.state !== "finalized") {
    throw new Error("expected a finalized report");
  }
  return finalizedReport.cases;
}

describe("PassGlyph", () => {
  it("says the outcome in words for every case of a report", () => {
    const entries = cases();
    expect(entries.filter((entry) => entry.passed)).toHaveLength(11);

    for (const entry of entries) {
      const { container } = render(() => <PassGlyph passed={entry.passed} />);
      const glyph = container.querySelector(".pass-glyph");
      expect(glyph).toHaveTextContent(entry.passed ? "passed" : "failed");
      expect(glyph).toHaveAttribute("data-tone", entry.passed ? "ok" : "fail");
      cleanup();
    }
  });

  it("draws §2.3's two marks, and keeps them away from assistive tech", () => {
    const failed = cases().find((entry) => !entry.passed);
    expect(failed?.caseId).toBe("backorders-when-out-of-stock");

    const { container } = render(() => <PassGlyph passed={failed?.passed ?? true} />);
    // a cross reads as nothing; the word is what has to reach a screen reader
    expect(container.querySelector("[aria-hidden='true']")).toHaveTextContent("✗");
    expect(container.querySelector(".visually-hidden")).toHaveTextContent("failed");
    cleanup();

    const passed = cases().find((entry) => entry.passed);
    const quiet = render(() => <PassGlyph passed={passed?.passed ?? false} />);
    expect(quiet.container.querySelector("[aria-hidden='true']")).toHaveTextContent("✓");
    expect(quiet.container.querySelector(".visually-hidden")).toHaveTextContent("passed");
  });
});
