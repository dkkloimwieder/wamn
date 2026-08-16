import type { JSX } from "@solidjs/web";

import type { Tone } from "./status";
import "./glyph.css";
import "./tone.css";

/**
 * The drawing behind §2.0's tree mark and §2.3's case mark: a toned character
 * that says nothing to a screen reader, so it is hidden, and the word beside it
 * that is the fact.
 *
 * `PassGlyph` and `VerdictGlyph` are both built from this, and neither is built
 * from the other. A case is pass/fail and an entity's verdict has four states —
 * two of which have no pass/fail reading at all — so the two vocabularies stay
 * apart even though the mark is one thing drawn twice. Nothing about either
 * vocabulary is decided here: the caller brings its own class, tone, mark and
 * word, and this only spells the structure they share.
 *
 * Every part is read inside this JSX, so a glyph handed a signal redraws —
 * §2.0's marks are cached display text "refreshed whenever the entity is
 * visited", and a row still wearing the verdict it was first seen with is the
 * one thing these marks exist not to do.
 */
export function Glyph(props: {
  class: string;
  tone: Tone;
  mark: string;
  word: string;
}): JSX.Element {
  return (
    <span class={props.class} data-tone={props.tone}>
      <span aria-hidden="true">{props.mark}</span>
      <span class="visually-hidden">{props.word}</span>
    </span>
  );
}
