import { cleanup, render } from "@solidjs/testing-library";
import { flush } from "solid-js";
import { afterEach, describe, expect, it } from "vitest";

import { finalizedReport } from "../reader/fixtures";
import type { AssertionFailure, ReportCase } from "../reader/types";
import { AssertionFailureView } from "./report-assertion";

/**
 * §2.3's expected/observed pair, family by family — and the vocabulary rule the
 * pair exists to keep: the durable word is the value, the wire word is a gloss,
 * and the gloss appears only where the two words differ or where there is no
 * wire word at all.
 */

afterEach(cleanup);

const cases: readonly ReportCase[] =
  finalizedReport.state === "finalized" ? finalizedReport.cases : [];

/** The fixture's one failing case carries one failure per family. */
const found = cases.find((reportCase) => !reportCase.passed);
if (found === undefined) {
  throw new Error("the finalized report fixture must carry a failing case");
}
const failing: ReportCase = found;

function assertionOf(family: string): AssertionFailure {
  const found = failing.failedAssertions.find(
    (assertion) => assertion.expected.family === family,
  );
  if (found === undefined) {
    throw new Error(`the failing case fixture carries no ${family} assertion`);
  }
  return found;
}

/**
 * What the pair draws: its text with the screen-reader-only asides removed. The
 * `aria-hidden` arrow stays, because §2.3 draws it and it is what a sighted
 * author reads on the line.
 */
function visible(element: Element): string {
  const copy = element.cloneNode(true) as Element;
  for (const aside of copy.querySelectorAll(".visually-hidden")) {
    aside.remove();
  }
  return (copy.textContent ?? "").replace(/\s+/g, " ").trim();
}

function view(failure: AssertionFailure, subject = "case c assertion 1") {
  const rendered = render(() => <AssertionFailureView failure={failure} subject={subject} />);
  flush();
  return rendered;
}

/** The expected side alone, so a gloss assertion cannot be satisfied by the observed text. */
function expected(container: HTMLElement): string {
  const found = container.querySelector(".report-expected");
  return found === null ? "" : visible(found);
}

const glosses = (container: HTMLElement): string[] =>
  [...container.querySelectorAll(".report-gloss")].map((gloss) => visible(gloss));

describe("the four assertion families", () => {
  it("renders run-terminal-outcome as the asserted status", () => {
    const { container } = view(assertionOf("run-terminal-outcome"));

    expect(expected(container)).toContain("run-terminal-outcome");
    expect(expected(container)).toContain("completed");
  });

  it("renders typed-flow-failure as the asserted flow failure kind", () => {
    const { container } = view(assertionOf("typed-flow-failure"));

    expect(expected(container)).toContain("typed-flow-failure");
    expect(expected(container)).toContain("retry-exhausted");
  });

  it("renders named-node-terminal as the flow/node pair, its status, and its kind", () => {
    const { container } = view(assertionOf("named-node-terminal"));

    // §2.3's `check-stock → error`, with the flow the node id belongs to
    expect(expected(container)).toContain("orders/check-stock → error");
    // the failure kind rides along, because the status is `error`
    expect(expected(container)).toContain("terminal");
  });

  it("renders terminal-respond as the status and the body, in a view that names it", () => {
    const { container, getByRole } = view(
      assertionOf("terminal-respond"),
      "case backorders-when-out-of-stock assertion 4",
    );

    expect(expected(container)).toContain("status 201");
    expect(container.querySelector(".json-view")).toBeInTheDocument();
    expect(container).toHaveTextContent("created");
    // the copy button says which evidence it acts on: one screen carries several
    getByRole("button", {
      name: "copy case backorders-when-out-of-stock assertion 4 expected body",
    });
  });

  it("prints the observed side verbatim, exactly as the evaluator wrote it", () => {
    for (const family of [
      "run-terminal-outcome",
      "typed-flow-failure",
      "named-node-terminal",
      "terminal-respond",
    ]) {
      const failure = assertionOf(family);
      const { container } = view(failure);
      // character for character, Rust debug formatting and all — never reflowed
      expect(container.querySelector(".report-observed")?.textContent).toBe(failure.observed);
      cleanup();
    }
  });
});

describe("the durable word is the value, the wire word is a gloss", () => {
  /** A local pair per case: the fixtures carry one assertion per family, not one per word. */
  const outcome = (status: "completed" | "failed" | "infrastructure-failure" | "effect-uncertain"): AssertionFailure => ({
    expected: { family: "run-terminal-outcome", status },
    observed: "run terminal status did not match",
  });

  it("glosses completed with the word the run screen will show", () => {
    const { container } = view(outcome("completed"));

    // the value is the durable word the author wrote
    expect(expected(container)).toContain("completed");
    expect(glosses(container)).toEqual(["(the run reads succeeded)"]);
    // and the tone is the wire word's, or a passing run would read grey
    expect(container.querySelector(".status-badge")).toHaveAttribute("data-tone", "ok");
  });

  it("glosses nothing where both vocabularies use the same word", () => {
    for (const status of ["failed", "effect-uncertain"] as const) {
      const { container } = view(outcome(status));

      expect(expected(container)).toContain(status);
      // the gloss that a screen printing it unconditionally would show
      expect(glosses(container)).toEqual([]);
      expect(container).not.toHaveTextContent("the run reads");
      cleanup();
    }
  });

  it("keeps infrastructure-failure's own word and names the absence of a wire one", () => {
    // no fixture asserts it — the wire vocabulary drops it entirely, which is
    // exactly why it is the word this screen must not round off to `failed`
    const { container } = view(outcome("infrastructure-failure"));

    expect(expected(container)).toContain("infrastructure-failure");
    expect(expected(container)).not.toContain("failed (");
    expect(glosses(container)[0]).toContain("no wire form");
    // no wire word, so no tone to borrow
    expect(container.querySelector(".status-badge")).toHaveAttribute("data-tone", "neutral");
  });

  it("names the budget kinds' absent wire form, and glosses the other kinds not at all", () => {
    const failure = (kind: "runaway-budget" | "depth-budget" | "retry-exhausted"): AssertionFailure => ({
      expected: { family: "typed-flow-failure", kind },
      observed: "no typed flow failure was captured",
    });

    for (const kind of ["runaway-budget", "depth-budget"] as const) {
      const { container } = view(failure(kind));
      expect(expected(container)).toContain(kind);
      expect(glosses(container)[0]).toContain("no wire form");
      cleanup();
    }

    const { container } = view(failure("retry-exhausted"));
    expect(expected(container)).toContain("retry-exhausted");
    expect(glosses(container)).toEqual([]);
  });
});

describe("what a named-node-terminal assertion does not carry", () => {
  it("shows no failure kind on a success, because the family forbids one", () => {
    const { container } = view({
      expected: {
        family: "named-node-terminal",
        flowId: "orders",
        nodeId: "check-stock",
        status: "success",
        failureKind: null,
      },
      observed: 'flow/node pair ("orders", "check-stock") had 1 mismatching occurrence(s) out of 1',
    });

    expect(expected(container)).toContain("orders/check-stock → success");
    expect(expected(container)).not.toContain("not recorded");
    expect(expected(container)).not.toContain("·");
  });

  it("says so when an error carries no failure kind, rather than showing nothing", () => {
    // the family requires one here, so its absence is a field the read did not
    // carry — which the line states instead of quietly dropping
    const { container } = view({
      expected: {
        family: "named-node-terminal",
        flowId: "orders",
        nodeId: "check-stock",
        status: "error",
        failureKind: null,
      },
      observed: "no failure kind was captured",
    });

    expect(expected(container)).toContain("orders/check-stock → error");
    expect(expected(container)).toContain("failure kind not recorded");
  });
});
