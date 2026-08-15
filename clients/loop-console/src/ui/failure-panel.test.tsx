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

describe("FailurePanel", () => {
  it("renders the failing run's kind, node, and detail as pairs", () => {
    const { container } = render(() => <FailurePanel failure={failureOf(failingRun.failure)} />);
    const panel = container.querySelector(".failure-panel");

    expect(panel).toHaveTextContent("kind");
    expect(panel).toHaveTextContent("retry-exhausted");
    expect(panel).toHaveTextContent("at");
    expect(panel).toHaveTextContent("fetch-inventory");
    expect(panel).toHaveTextContent("detail");
    expect(panel).toHaveTextContent("upstream 503 after 4 attempts");
    expect(panel).not.toHaveTextContent("not recorded");
  });

  it("keeps the raw failure behind a disclosure, and opens it into a JsonView", () => {
    const { container, getByRole } = render(() => (
      <FailurePanel failure={failureOf(failingRun.failure)} />
    ));

    const toggle = getByRole("button", { name: "raw failure" });
    expect(toggle).toHaveAttribute("aria-expanded", "false");
    expect(container.querySelector(".json-tree")).toBeNull();

    fireEvent.click(toggle);
    flush();

    expect(toggle).toHaveAttribute("aria-expanded", "true");
    const tree = container.querySelector(".json-tree");
    expect(tree).toHaveTextContent('"attempts": 4');
    expect(tree).toHaveTextContent('"node": "fetch-inventory"');
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
    const pairs = container.querySelectorAll(".key-value");
    expect(pairs[2]).toHaveTextContent("detail");
    expect(pairs[2]).toHaveTextContent("not recorded");
  });

  it("states an absent failing node too, which no fixture carries", () => {
    // a shape, not domain data: every run fixture names the node it failed at,
    // but `runs.fail_node` is nullable and the panel must not blank it
    const nodeless: RunFailure = { kind: "deadline-exhausted", node: null, detail: null, raw: null };
    const { container } = render(() => <FailurePanel failure={nodeless} />);

    const pairs = container.querySelectorAll(".key-value");
    expect(pairs[1]).toHaveTextContent("at");
    expect(pairs[1]).toHaveTextContent("not recorded");
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
