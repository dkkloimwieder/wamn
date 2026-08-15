import type { JSX } from "@solidjs/web";

import "./pass-glyph.css";
import "./tone.css";

/**
 * §2.0's tree mark and §2.3's case mark. The glyph is decoration — a check and a
 * cross say nothing to a screen reader — so it is hidden and the word beside it
 * is the fact.
 */
export function PassGlyph(props: { passed: boolean }): JSX.Element {
  return (
    <span class="pass-glyph" data-tone={props.passed ? "ok" : "fail"}>
      <span aria-hidden="true">{props.passed ? "✓" : "✗"}</span>
      <span class="visually-hidden">{props.passed ? "passed" : "failed"}</span>
    </span>
  );
}
