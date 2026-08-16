import { cleanup, fireEvent, render } from "@solidjs/testing-library";
import { createSignal, flush } from "solid-js";
import { afterEach, describe, expect, it } from "vitest";

import { effectUncertainRun, failingRun } from "../reader/fixtures";
import type { RunFailure } from "../reader/types";
import { FailurePanel } from "./failure-panel";

afterEach(cleanup);

/** Both runs carry one; the panel's whole subject is a failure that exists. */
function failureOf(failure: RunFailure | null): RunFailure {
  if (failure === null) {
    throw new Error("fixture carries no run failure");
  }
  return failure;
}

function text(node: Element | null): string {
  return (node?.textContent ?? "").replace(/\s+/g, " ").trim();
}

/**
 * Every pair the panel prints, label beside its own value.
 *
 * Text over the whole panel would pass with the values dealt to the wrong
 * labels — `at retry-exhausted` says the run failed at a node called
 * `retry-exhausted` — and the pairing is the only thing that makes a value
 * readable at all.
 */
function pairs(container: HTMLElement): ReadonlyArray<readonly [string, string]> {
  return Array.from(
    container.querySelectorAll(".key-value"),
    (pair): readonly [string, string] => [
      text(pair.querySelector("dt")),
      text(pair.querySelector("dd")),
    ],
  );
}

describe("FailurePanel", () => {
  it("renders the failing run's kind, node, and detail as pairs", () => {
    const { container } = render(() => <FailurePanel failure={failureOf(failingRun.failure)} />);

    expect(pairs(container)).toEqual([
      ["kind", "retry-exhausted"],
      ["at", "fetch-inventory"],
      ["detail", "upstream 503 after 4 attempts"],
    ]);
    expect(container.querySelector(".failure-panel")).not.toHaveTextContent("not recorded");
  });

  it("keeps the raw failure behind a disclosure, and opens it into a JsonView", () => {
    const { container, getByRole } = render(() => (
      <FailurePanel failure={failureOf(failingRun.failure)} />
    ));

    const toggle = getByRole("button", { name: "raw failure" });
    expect(toggle).toHaveAttribute("aria-expanded", "false");
    expect(container.querySelector(".json-tree")).toBeNull();
    // closed is empty, not hidden: nothing of the raw failure exists to be read
    expect(container.querySelector(".disclosure-panel")).toBeNull();

    fireEvent.click(toggle);
    flush();

    expect(toggle).toHaveAttribute("aria-expanded", "true");
    const tree = container.querySelector(".json-tree");
    expect(tree).toHaveTextContent('"attempts": 4');
    expect(tree).toHaveTextContent('"node": "fetch-inventory"');
  });

  it("trails the detail with the raw toggle, on the detail's row and no other", () => {
    // §2.2's wireframe puts `▸ raw` at the end of the detail line. jsdom lays
    // nothing out, so what is provable here is the grouping the line is made of:
    // the toggle shares the detail's row, and `kind` and `at` keep theirs.
    const { container, getByRole } = render(() => (
      <FailurePanel failure={failureOf(failingRun.failure)} />
    ));

    const row = getByRole("button", { name: "raw failure" }).closest(".failure-detail-row");
    expect(row).not.toBeNull();
    expect(row).toHaveTextContent("detail");
    expect(row).toHaveTextContent("upstream 503 after 4 attempts");
    expect(row).not.toHaveTextContent("retry-exhausted");
    expect(row).not.toHaveTextContent("fetch-inventory");

    // and the pair itself still says only what `runs.fail_reason` recorded: the
    // raw failure is evidence beside the detail, never part of its value
    expect(pairs(container)[2]).toEqual(["detail", "upstream 503 after 4 attempts"]);
  });

  it("opens the raw beneath the row it trails", () => {
    const { container, getByRole } = render(() => (
      <FailurePanel failure={failureOf(failingRun.failure)} />
    ));

    const toggle = getByRole("button", { name: "raw failure" });
    fireEvent.click(toggle);
    flush();

    // the evidence stays attached to the detail it was opened from: the tree
    // exists, and it hangs inside the very row the toggle sits in. `contains`
    // rather than a query off the row, so a toggle that escaped the row (which
    // makes `closest` null) fails here instead of quietly asserting nothing.
    const tree = container.querySelector(".json-tree");
    expect(tree).not.toBeNull();
    expect(toggle.closest(".failure-detail-row")?.contains(tree)).toBe(true);
  });

  it("names the opened raw's controls after the evidence they act on", () => {
    // §2.2 puts more JsonViews on this one screen — a captured output per
    // disclosed fact — and §3's floor forbids two controls sharing a name. The
    // subject this panel hands JsonView is the whole of what keeps these
    // unique, so it is load-bearing behaviour and not decoration.
    const { getByRole } = render(() => <FailurePanel failure={failureOf(failingRun.failure)} />);

    fireEvent.click(getByRole("button", { name: "raw failure" }));
    flush();

    expect(getByRole("button", { name: "copy the raw failure" })).toBeInTheDocument();
    const nested = getByRole("button", { name: "collapse the raw failure.last" });

    // and it keeps naming this panel's evidence as it moves
    fireEvent.click(nested);
    flush();
    expect(getByRole("button", { name: "expand the raw failure.last" })).toBeInTheDocument();
  });

  it("still trails an unrecorded detail with the raw, because the two are independent", () => {
    // a shape, not domain data: no fixture pairs a null `runs.fail_reason` with
    // a raw failure, but the wire carries them separately and a missing reason
    // must not withdraw evidence the read did return
    const detailless: RunFailure = {
      kind: "terminal",
      node: "charge-card",
      detail: null,
      raw: { kind: "terminal", node: "charge-card" },
    };
    const { getByRole } = render(() => <FailurePanel failure={detailless} />);

    const row = getByRole("button", { name: "raw failure" }).closest(".failure-detail-row");
    expect(row).toHaveTextContent("detail");
    expect(row).toHaveTextContent("not recorded");
  });

  it("offers no raw disclosure when the effect-uncertain run carries no raw", () => {
    const failure = failureOf(effectUncertainRun.failure);
    const { container, queryByRole } = render(() => <FailurePanel failure={failure} />);

    expect(container.querySelector(".failure-panel")).toHaveTextContent("effect-uncertain");
    expect(container.querySelector(".failure-panel")).toHaveTextContent("charge-card");
    // a null raw is no disclosure, not an empty one
    expect(queryByRole("button")).toBeNull();
  });

  it("states each absence rather than leaving it blank", () => {
    const { container } = render(() => (
      <FailurePanel failure={failureOf(effectUncertainRun.failure)} />
    ));

    // the detail column of `runs` is null here, and a blank would read as none
    expect(pairs(container)[2]).toEqual(["detail", "not recorded"]);

    // §1.2: `not recorded` is the console's own word about the read, not
    // something the read returned. In the data face it would sit in the same
    // column as `upstream 503 after 4 attempts` and read as a recorded value.
    expect(container.querySelector(".failure-absent")).toHaveClass("frame");
  });

  it("states an absent failing node too, which no fixture carries", () => {
    // a shape, not domain data: every run fixture names the node it failed at,
    // but `runs.fail_node` is nullable and the panel must not blank it
    const nodeless: RunFailure = { kind: "deadline-exhausted", node: null, detail: null, raw: null };
    const { container } = render(() => <FailurePanel failure={nodeless} />);

    expect(pairs(container)[1]).toEqual(["at", "not recorded"]);
  });

  it("reads a blank fail_node or fail_reason as no record, not as an empty value", () => {
    // a shape, not domain data: both are plain nullable text columns and nothing
    // between the database and here rejects the empty string, so `""` reaches
    // this panel as a legal value. Printed as itself it is a blank beside `at` —
    // a run that failed nowhere — which is the claim the panel refuses to make.
    const blank: RunFailure = { kind: "terminal", node: "", detail: "   ", raw: null };
    const { container } = render(() => <FailurePanel failure={blank} />);

    expect(pairs(container)).toEqual([
      ["kind", "terminal"],
      ["at", "not recorded"],
      ["detail", "not recorded"],
    ]);
  });

  it("moves every pair when a refresh re-reads the screen into another failure", () => {
    // §2.6's refresh re-issues the screen's one read. A pair that froze on the
    // previous run would leave this panel naming a node this failure never hit.
    const [failure, setFailure] = createSignal(failureOf(failingRun.failure));
    const { container } = render(() => <FailurePanel failure={failure()} />);
    const panel = container.querySelector(".failure-panel");
    expect(panel).toHaveTextContent("fetch-inventory");

    setFailure(failureOf(effectUncertainRun.failure));
    flush();

    expect(panel).toHaveTextContent("effect-uncertain");
    expect(panel).toHaveTextContent("charge-card");
    expect(panel).not.toHaveTextContent("fetch-inventory");
    expect(panel).not.toHaveTextContent("upstream 503 after 4 attempts");
    // the detail the second run does not carry is spelled out, not left stale
    expect(panel).toHaveTextContent("not recorded");
  });
});
