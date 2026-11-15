// @vitest-environment jsdom
import { makeInteractOutside } from "@solid-primitives/interaction";
import { expect, it, vi } from "vitest";

it("reports outside focus before the pointer listener starts", () => {
  vi.useFakeTimers();
  const inside = document.body.appendChild(document.createElement("div"));
  const outside = document.body.appendChild(document.createElement("button"));
  const onFocusOutside = vi.fn();
  const dispose = makeInteractOutside(inside, { onFocusOutside });
  try {
    outside.focus();
    expect(onFocusOutside).toHaveBeenCalledTimes(1);
  } finally {
    dispose();
    inside.remove();
    outside.remove();
    vi.useRealTimers();
  }
});
