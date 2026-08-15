import { cleanup, render } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";

import { failingRun } from "../reader/fixtures";
import { KeyValue } from "./key-value";

afterEach(cleanup);

/** §2.2's failure panel, pair by pair. */
function pairs(): ReadonlyArray<{ label: string; value: string }> {
  const failure = failingRun.failure;
  if (failure === null || failure.node === null || failure.detail === null) {
    throw new Error("expected the failing run to carry a kind, a node, and a detail");
  }
  return [
    { label: "kind", value: failure.kind },
    { label: "at", value: failure.node },
    { label: "detail", value: failure.detail },
  ];
}

describe("KeyValue", () => {
  it("renders each pair of the run failure panel, its key out of the divider role", () => {
    for (const pair of pairs()) {
      const { container } = render(() => <KeyValue label={pair.label}>{pair.value}</KeyValue>);
      expect(container.querySelector("dt")).toHaveTextContent(pair.label);
      // `.label` is §1.4's uppercase section divider. §2.2 draws these keys
      // lowercase in the data face, so wearing that role would spell them
      // KIND / AT / DETAIL — a difference jsdom computes no styles to catch.
      expect(container.querySelector("dt")).not.toHaveClass("label");
      expect(container.querySelector("dd")).toHaveTextContent(pair.value);
      cleanup();
    }
  });

  it("associates the value with its label rather than leaving them adjacent", () => {
    const [pair] = pairs();
    const { container } = render(() => <KeyValue label={pair.label}>{pair.value}</KeyValue>);
    // the pair owns its list, so the association holds wherever a panel drops it
    const list = container.querySelector("dl");
    expect(list?.querySelector("dt")).toHaveTextContent(pair.label);
    expect(list?.querySelector("dd")).toHaveTextContent(pair.value);
  });
});
