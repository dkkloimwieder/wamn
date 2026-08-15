import { cleanup, render } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";

import { captureOffRun, truncatedRun } from "../reader/fixtures";
import { EmptyState, LoadingBlock } from "./states";

afterEach(cleanup);

/** What the wireframe prints: the reason alone, without the spoken region. */
function printed(empty: HTMLElement): string {
  const spoken = empty.querySelector(".visually-hidden")?.textContent ?? "";
  return (empty.textContent ?? "").slice(spoken.length);
}

describe("LoadingBlock", () => {
  it("names the region it stands in, and announces it", () => {
    const { getByRole } = render(() => <LoadingBlock region={`run ${truncatedRun.runId}`} />);

    // there is no spinner to see instead, so both announcements carry it
    const block = getByRole("status");
    expect(block).toHaveTextContent(`loading run ${truncatedRun.runId}…`);
    expect(block).toHaveAttribute("aria-busy", "true");
    expect(block).toHaveClass("frame");
  });
});

describe("EmptyState", () => {
  it("prints §2.2's capture-off output slot as the spec spells it", () => {
    // the reason is the run's own capture policy, not a guess about it
    const { container } = render(() => (
      <EmptyState region="output" reason={`capture was ${captureOffRun.capture} for this run`} />
    ));

    const empty = container.querySelector<HTMLElement>(".empty-state") as HTMLElement;
    expect(printed(empty)).toBe("capture was off for this run");
    expect(empty).toHaveClass("frame");
    // the region is spoken, so a reader out of sight of the section label has it
    expect(empty.querySelector(".visually-hidden")).toHaveTextContent("output");
  });

  it("prints §2.0's never-visited tree without repeating its kind label", () => {
    const { container } = render(() => <EmptyState region="runs" reason="nothing visited yet" />);

    const empty = container.querySelector<HTMLElement>(".empty-state") as HTMLElement;
    expect(printed(empty)).toBe("nothing visited yet");
    expect(empty.querySelector(".visually-hidden")).toHaveTextContent("runs");
  });
});
