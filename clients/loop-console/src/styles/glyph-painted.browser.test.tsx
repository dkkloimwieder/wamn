import { render } from "@solidjs/testing-library";
import { flush } from "solid-js";
import { describe, expect, it } from "vitest";

import { PassGlyph } from "../ui/pass-glyph";
import { VerdictGlyph } from "../ui/verdict-glyph";

/**
 * The one claim the fold onto a shared `Glyph` (wamn-dggp.31) left unprovable in
 * jsdom: `pass-glyph.css` and `verdict-glyph.css` were one rule each, written
 * twice, and are now one rule in `glyph.css` that the shared component imports
 * instead of each caller. jsdom parses no stylesheet, so nothing in the fast
 * suite can say whether the surviving sheet still reaches either mark — a fold
 * that dropped an import would leave both glyphs uncoloured and every jsdom
 * assertion about them would still pass.
 *
 * §1.1's rule is what is under test: the tone reaches the mark through
 * `var(--tone)`, and the four verdicts are four different colours rather than
 * one inherited text colour wearing four names.
 *
 * Beside `painted.browser.test.tsx` rather than beside the glyphs it renders,
 * and that placement is load-bearing: browser mode writes a `__screenshots__`
 * directory next to a failing test file, and `accessibility-floor.test.tsx`
 * reads every entry of `src/ui/` as a file. A browser proof kept there would,
 * the first time it went red, take the whole accessibility floor down with it
 * on an `EISDIR` that names nothing to do with either.
 */

function painted(element: Element): string {
  return getComputedStyle(element).color;
}

function glyph(node: Element | null): Element {
  expect(node).not.toBeNull();
  return node as Element;
}

describe("§1.1's tone reaches both marks after the fold", () => {
  it("colours §2.3's case glyph by its outcome", () => {
    const { container } = render(() => (
      <>
        <PassGlyph passed={true} />
        <PassGlyph passed={false} />
      </>
    ));
    flush();

    const [ok, fail] = [...container.querySelectorAll(".pass-glyph")].map((mark) =>
      painted(glyph(mark)),
    );
    // the sheet is loaded and `--tone` resolved: an unstyled span reports the
    // body text colour for both, which is the failure this test exists to catch
    expect(ok).not.toBe(fail);
    expect(painted(container)).not.toBe(ok);
    expect(painted(container)).not.toBe(fail);
  });

  it("colours §2.0's four entity verdicts apart", () => {
    const { container } = render(() => (
      <>
        <VerdictGlyph state="ok" />
        <VerdictGlyph state="fail" />
        <VerdictGlyph state="uncertain" />
        <VerdictGlyph state="none" />
      </>
    ));
    flush();

    const colours = [...container.querySelectorAll(".verdict-glyph")].map((mark) =>
      painted(glyph(mark)),
    );
    expect(colours).toHaveLength(4);
    // `none` is `neutral` and the other three are their own status colours, so
    // four states must paint four ways — §2.0's `◌` sharing `✗`'s red would be
    // the misread §2.2's uncertain panel exists to prevent
    expect(new Set(colours).size).toBe(4);
  });

  it("gives the two vocabularies the same colour for the same tone", () => {
    // the fold's other half: one rule now paints both, so `ok` is `ok` whichever
    // glyph draws it. A selector that had kept only one class alive would show
    // up here as a case mark and an entity mark disagreeing about one tone.
    const { container } = render(() => (
      <>
        <PassGlyph passed={true} />
        <VerdictGlyph state="ok" />
        <PassGlyph passed={false} />
        <VerdictGlyph state="fail" />
      </>
    ));
    flush();

    expect(painted(glyph(container.querySelector(".pass-glyph")))).toBe(
      painted(glyph(container.querySelector(".verdict-glyph"))),
    );
    const [, failedCase] = container.querySelectorAll(".pass-glyph");
    const [, failedEntity] = container.querySelectorAll(".verdict-glyph");
    expect(painted(glyph(failedCase))).toBe(painted(glyph(failedEntity)));
  });
});
