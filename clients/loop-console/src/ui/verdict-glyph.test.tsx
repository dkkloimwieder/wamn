import { cleanup, render } from "@solidjs/testing-library";
import { createSignal, flush } from "solid-js";
import { afterEach, describe, expect, it } from "vitest";

import {
  effectUncertainRun,
  failingRun,
  parsingDraft,
  passingRun,
  pendingReport,
  terminalizedRun,
} from "../reader/fixtures";
import {
  reportVerdictState,
  runVerdictState,
  visitFromDraft,
  visitFromReport,
  visitFromRun,
} from "../store/visited";
import { PassGlyph } from "./pass-glyph";
import { type VerdictState, verdictTone } from "./status";
import { VerdictGlyph } from "./verdict-glyph";

afterEach(cleanup);

/**
 * Typed as a full record so a fifth verdict state could not be added to the
 * vocabulary without this file refusing to compile — the point of the glyph is
 * that it draws *every* state §2.0 can put in the tree.
 */
const expectedWord: Readonly<Record<VerdictState, string>> = {
  ok: "verdict ok",
  fail: "verdict fail",
  uncertain: "verdict effect-uncertain",
  none: "no verdict",
};

const states = Object.keys(expectedWord) as readonly VerdictState[];

/**
 * What a reader reaches on this element: its text with every aria-hidden
 * subtree removed. jsdom computes no accessible name, so this is the same
 * approximation §3's floor makes — strict on purpose, because markup a reader
 * never announces must not be able to satisfy an assertion here.
 */
function reachableText(element: Element): string {
  const copy = element.cloneNode(true) as Element;
  for (const hidden of copy.querySelectorAll('[aria-hidden="true"]')) {
    hidden.remove();
  }
  return (copy.textContent ?? "").replace(/\s+/g, " ").trim();
}

/**
 * What a sighted reader sees on this element: its text with every
 * visually-hidden subtree removed. jsdom computes no styles, so the class
 * `app.css` clips with is the only handle on "off-screen" there is — the same
 * handle `PassGlyph`'s test uses. Without this the two readings are
 * indistinguishable and the label could go visible unnoticed: §2.0 draws one
 * mark and an ellipsized id in 260px, so a row that also printed `verdict ok`
 * would be a different panel.
 */
function visibleText(element: Element): string {
  const copy = element.cloneNode(true) as Element;
  for (const hidden of copy.querySelectorAll(".visually-hidden")) {
    hidden.remove();
  }
  return (copy.textContent ?? "").replace(/\s+/g, " ").trim();
}

function glyph(state: VerdictState): Element {
  const { container } = render(() => <VerdictGlyph state={state} />);
  const rendered = container.querySelector(".verdict-glyph");
  expect(rendered).not.toBeNull();
  return rendered as Element;
}

describe("VerdictGlyph", () => {
  it("draws all four of §2.0's states, each one labelled", () => {
    expect(states).toHaveLength(4);
    for (const state of states) {
      expect(reachableText(glyph(state))).toBe(expectedWord[state]);
      cleanup();
    }
  });

  it("labels the states the platform actually produces, not invented ones", () => {
    // every state below is one the recents store derives from a step-2 fixture,
    // so the glyph is proven against verdicts the platform hands over
    const derived: ReadonlyArray<readonly [VerdictState, string]> = [
      [runVerdictState(passingRun), "verdict ok"],
      [runVerdictState(failingRun), "verdict fail"],
      [runVerdictState(effectUncertainRun), "verdict effect-uncertain"],
      // a terminalized run reads back as `failed`; §2.0 keeps showing `◌`
      [runVerdictState(terminalizedRun), "verdict effect-uncertain"],
      // a report that has not finalized, and a draft, have no verdict at all
      [reportVerdictState(pendingReport), "no verdict"],
      [visitFromDraft(parsingDraft, 0).verdict, "no verdict"],
      [visitFromRun(failingRun, 0).verdict, "verdict fail"],
      [visitFromReport(pendingReport, 0).verdict, "no verdict"],
    ];

    for (const [state, word] of derived) {
      expect(reachableText(glyph(state))).toBe(word);
      cleanup();
    }
  });

  it("takes the tone the verdict vocabulary assigns, and names no colour", () => {
    for (const state of states) {
      expect(glyph(state)).toHaveAttribute("data-tone", verdictTone(state));
      cleanup();
    }

    // pinned, because these two are the states most easily mistaken for each
    // other's neighbours: `uncertain` is §2.0's one purple mark, and `none` is
    // a real state that must not borrow `fail`'s colour
    expect(glyph("uncertain")).toHaveAttribute("data-tone", "uncertain");
    cleanup();
    expect(glyph("none")).toHaveAttribute("data-tone", "neutral");
  });

  it("never leaves the colour to carry the verdict on its own", () => {
    for (const state of states) {
      const rendered = glyph(state);
      // the floor's rule, applied here: every toned element says something a
      // reader reaches, in letters
      expect(rendered).toHaveAttribute("data-tone");
      expect(reachableText(rendered)).toMatch(/\p{L}/u);
      cleanup();
    }
  });

  it("hides the mark itself from the accessible name", () => {
    const marks: Readonly<Record<VerdictState, string>> = {
      ok: "✓",
      fail: "✗",
      uncertain: "◌",
      none: "•",
    };

    for (const state of states) {
      const rendered = glyph(state);
      // the mark is drawn, and it is drawn inside the hidden subtree — a check,
      // a cross, a hollow circle and a dot all read as nothing
      expect(rendered.querySelector('[aria-hidden="true"]')).toHaveTextContent(marks[state]);
      expect(reachableText(rendered)).not.toContain(marks[state]);
      cleanup();
    }
  });

  it("keeps the word off the tree row, so only the mark is drawn", () => {
    const marks: Readonly<Record<VerdictState, string>> = {
      ok: "✓",
      fail: "✗",
      uncertain: "◌",
      none: "•",
    };

    for (const state of states) {
      const rendered = glyph(state);
      // the whole row's visible ink is the one character; the label is reached
      // only by assistive tech, which is what makes it safe to spell it out
      expect(visibleText(rendered)).toBe(marks[state]);
      expect(visibleText(rendered)).not.toContain(expectedWord[state]);
      // and the word is still there for a reader who cannot see the mark
      expect(reachableText(rendered)).toBe(expectedWord[state]);
      cleanup();
    }
  });

  it("redraws when the entity it marks moves to another verdict", () => {
    // §2.0's glyphs are cached display text "refreshed whenever the entity is
    // visited", and the visited store is a signal — a row still saying `•` for
    // a run that has since failed is the one thing this mark exists not to do
    const [state, setState] = createSignal<VerdictState>("none");
    const { container } = render(() => <VerdictGlyph state={state()} />);
    flush();

    const rendered = (): Element => container.querySelector(".verdict-glyph") as Element;
    expect(reachableText(rendered())).toBe("no verdict");
    expect(visibleText(rendered())).toBe("•");
    expect(rendered()).toHaveAttribute("data-tone", "neutral");

    setState(runVerdictState(failingRun));
    flush();
    expect(reachableText(rendered())).toBe("verdict fail");
    expect(visibleText(rendered())).toBe("✗");
    expect(rendered()).toHaveAttribute("data-tone", "fail");

    setState(runVerdictState(effectUncertainRun));
    flush();
    expect(reachableText(rendered())).toBe("verdict effect-uncertain");
    expect(visibleText(rendered())).toBe("◌");
    expect(rendered()).toHaveAttribute("data-tone", "uncertain");
  });

  it("says a state it does not recognize rather than drawing a blank mark", () => {
    // The type closes this path and the runtime does not: the wire types run
    // status as a bare string (the STEP-9 RISK in `reader/types.ts`), and
    // `runVerdictState` has no default arm, so an unheard-of status becomes a
    // verdict state no vocabulary here has a word for.
    const unheardOf = glyph("settled-by-counterparty" as VerdictState);
    expect(reachableText(unheardOf)).toContain("not recognized");
    // rendered as it arrived, the way `StatusBadge` renders an unknown status
    expect(reachableText(unheardOf)).toContain("settled-by-counterparty");
    expect(visibleText(unheardOf)).not.toBe("");
    // unrecognized is not a verdict of its own, so it takes no status colour
    expect(unheardOf).toHaveAttribute("data-tone", "neutral");
    cleanup();

    // the same path with nothing to quote — the absent state, which is what a
    // missing default arm actually hands over
    const missing = glyph(undefined as unknown as VerdictState);
    expect(reachableText(missing)).toMatch(/\p{L}/u);
    expect(reachableText(missing)).toContain("not recognized");
    expect(reachableText(missing)).not.toContain("undefined");
    expect(visibleText(missing)).not.toBe("");
  });

  it("does not borrow §2.3's case words for an entity's verdict", () => {
    const said = states.map((state) => reachableText(glyph(state)));
    cleanup();
    // four states, four different things to say
    expect(new Set(said).size).toBe(4);

    const cases = [true, false].map((passed) => {
      const { container } = render(() => <PassGlyph passed={passed} />);
      const word = reachableText(container.querySelector(".pass-glyph") as Element);
      cleanup();
      return word;
    });
    expect(cases).toEqual(["passed", "failed"]);
    // an entity's verdict is not a case outcome: `uncertain` and `none` have no
    // pass/fail reading at all, so no label here may collide with one
    for (const word of said) {
      expect(cases).not.toContain(word);
    }
  });
});
