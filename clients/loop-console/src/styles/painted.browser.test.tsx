import { render } from "@solidjs/testing-library";
import { flush } from "solid-js";
import { describe, expect, it } from "vitest";

import { VerdictBar } from "../ui/verdict-bar";

/**
 * The first proofs that could only ever be made in a browser (wamn-dggp.16).
 *
 * Every assertion here is one the jsdom suite has been standing in for with a
 * source lint — `accessibility-floor.test.tsx` reads `app.css` as *text* and
 * checks that exactly one focus ring is declared and that no primitive erases
 * it, because jsdom parses no stylesheet and so cannot be asked whether a ring
 * is drawn. A lint over the source proves the rule was written. Only a browser
 * proves it reaches the element.
 *
 * These are deliberately about paint and layout, not behaviour: behaviour
 * belongs in the fast jsdom project.
 */

describe("the focus ring reaches a focused control", () => {
  it("draws §3's one ring on a control the keyboard moved to", () => {
    const { container } = render(() => (
      <button type="button" id="ringed">
        focus me
      </button>
    ));
    flush();
    const button = container.querySelector<HTMLButtonElement>("#ringed");
    expect(button).not.toBeNull();

    // `:focus-visible` answers to *how* focus arrived — a script calling
    // `.focus()` is not a keyboard, and a plain call would match nothing here
    // and quietly prove the opposite of what this test claims. `focusVisible`
    // is the standard option that asks for the keyboard's treatment explicitly.
    button?.focus({ focusVisible: true } as FocusOptions);

    const painted = getComputedStyle(button as Element);
    expect(button?.matches(":focus-visible")).toBe(true);
    // 2px solid var(--signal): the width is the part that proves it was painted
    // rather than merely matched, and `none`/`0px` is what a suppressed ring
    // would report.
    expect(painted.outlineStyle).toBe("solid");
    expect(Number.parseFloat(painted.outlineWidth)).toBeGreaterThan(0);
    expect(painted.outlineColor).not.toBe("");
    // §3 sets the offset so the ring clears the control it belongs to.
    expect(Number.parseFloat(painted.outlineOffset)).toBeGreaterThan(0);
  });
});

describe("§1.3's verdict rule", () => {
  it("paints the 3px rule the spec calls the console's signature", () => {
    const { container } = render(() => <VerdictBar tone="fail" verdict="FAILED" />);
    flush();
    const rule = container.querySelector(".verdict-rule");
    expect(rule).not.toBeNull();

    // A `::before` with `content: ""` — jsdom has no pseudo-element at all, so
    // nothing in the fast suite can state that the console's signature renders.
    const before = getComputedStyle(rule as Element, "::before");
    expect(before.content).not.toBe("none");
    expect(before.height).toBe("3px");
    // `flex: 1` — the rule runs out to fill the bar, which is the whole shape of
    // §1.3. A zero width would be a hairline that is declared and invisible.
    expect(Number.parseFloat(before.width)).toBeGreaterThan(0);
  });
});
