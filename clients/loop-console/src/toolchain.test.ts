import { createMemo, createRoot, createSignal, flush, type Setter } from "solid-js";
import { describe, expect, it } from "vitest";

describe("Solid toolchain", () => {
  it("propagates a signal through a memo", () => {
    let setValue!: Setter<number>;
    let doubled!: () => number;

    const dispose = createRoot((dispose) => {
      const [value, set] = createSignal(1);
      setValue = set;
      doubled = createMemo(() => value() * 2);
      return dispose;
    });

    expect(doubled()).toBe(2);
    // The write sits outside the owned scope on purpose: Solid 2 refuses
    // reactive writes inside a component or computation.
    setValue(2);
    flush();
    expect(doubled()).toBe(4);

    dispose();
  });
});
