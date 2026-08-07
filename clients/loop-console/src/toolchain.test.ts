import { createRoot, createSignal } from "solid-js";
import { describe, expect, it } from "vitest";

describe("Solid toolchain", () => {
  it("runs reactive computations", () => {
    createRoot((dispose) => {
      const [value, setValue] = createSignal(1);
      setValue(2);
      expect(value()).toBe(2);
      dispose();
    });
  });
});
