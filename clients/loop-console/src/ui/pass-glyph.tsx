import type { JSX } from "@solidjs/web";

import { Glyph } from "./glyph";

/**
 * §2.0's tree mark and §2.3's case mark. The glyph is decoration — a check and a
 * cross say nothing to a screen reader — so it is hidden and the word beside it
 * is the fact.
 *
 * `Glyph` draws it, the same construction `VerdictGlyph` is drawn from. The two
 * are kept apart at the word: these two say "passed" / "failed" about one test
 * case, which is a reading only §2.3 has.
 */
export function PassGlyph(props: { passed: boolean }): JSX.Element {
  return (
    <Glyph
      class="pass-glyph"
      tone={props.passed ? "ok" : "fail"}
      mark={props.passed ? "✓" : "✗"}
      word={props.passed ? "passed" : "failed"}
    />
  );
}
