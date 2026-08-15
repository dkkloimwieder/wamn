import { cleanup, render } from "@solidjs/testing-library";
import { afterEach, describe, expect, it } from "vitest";

import { runFixtures } from "../reader/fixtures";
import type { RunStatus } from "../reader/types";
import { nodeRunTone, runTone } from "./status";
import type { Tone } from "./status";
import { StatusBadge } from "./status-badge";

afterEach(cleanup);

function badge(status: string, tone: Tone): Element | null {
  const { container } = render(() => <StatusBadge status={status} tone={tone} />);
  return container.querySelector(".status-badge");
}

const runs = [...runFixtures.values()];

describe("StatusBadge", () => {
  it("renders every run status the fixtures carry, word and tone together", () => {
    const statuses = new Set(runs.map((run) => run.status));
    expect(statuses).toEqual(new Set(["succeeded", "failed", "effect-uncertain"]));

    for (const status of statuses) {
      const rendered = badge(status, runTone(status));
      // exact, not substring: verbatim is this primitive's one promise
      expect(rendered?.textContent).toBe(status);
      expect(rendered).toHaveAttribute("data-tone", runTone(status));
      cleanup();
    }
  });

  it("renders the three run statuses no finished run can be in", () => {
    // queued, dispatched and running are pre-terminal, so no canned read sits in one
    const pending: RunStatus[] = ["queued", "dispatched", "running"];
    for (const status of pending) {
      const rendered = badge(status, runTone(status));
      expect(rendered?.textContent).toBe(status);
      expect(rendered).toHaveAttribute("data-tone", "neutral");
      cleanup();
    }
  });

  it("renders every node run status the facts carry", () => {
    const statuses = new Set(runs.flatMap((run) => run.facts).map((fact) => fact.status));
    expect(statuses).toEqual(new Set(["started", "success", "error"]));

    for (const status of statuses) {
      const rendered = badge(status, nodeRunTone(status));
      expect(rendered?.textContent).toBe(status);
      expect(rendered).toHaveAttribute("data-tone", nodeRunTone(status));
      cleanup();
    }
  });

  it("renders an unheard-of word as itself, uncoloured", () => {
    // the durable vocabulary the wire drops, which a widened projection could send back
    const unheard = "infrastructure-failure";
    const rendered = badge(unheard, runTone(unheard));
    expect(rendered?.textContent).toBe(unheard);
    expect(rendered).toHaveAttribute("data-tone", "neutral");
  });
});
