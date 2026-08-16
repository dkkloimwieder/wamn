import type { JSX } from "@solidjs/web";

import { Glyph } from "./glyph";
import { type VerdictState, verdictTone } from "./status";

/**
 * §2.0's tree mark: the verdict an *entity* carries, in the four states the
 * panel draws. Drawn by `Glyph`, the one construction `PassGlyph` is drawn from
 * too — the character is decoration and is hidden, the word beside it is the
 * fact, and the tone is set from the vocabulary rather than named here. The
 * drawing is folded; the vocabulary is not, because the two say different
 * things about different objects.
 *
 * The words are deliberately not `PassGlyph`'s. That glyph says "passed" /
 * "failed" about one test case, which is a reading only §2.3 has; here the
 * subject is a run, a report or a draft, and two of the four states have no
 * pass/fail reading at all — a run whose effect is unknown has neither passed
 * nor failed, and a draft has no verdict to pass or fail. So each label names
 * the verdict as the verdict vocabulary itself spells it, and the absent state
 * says the absence in words ("no verdict") rather than borrowing a nearby one:
 * a draft never gets a verdict and a report gets one only once it finalizes, so
 * anything with "yet" or "pending" in it would be a claim about one of them.
 *
 * `verdict effect-uncertain` covers a terminalized run as well as an
 * outstanding one, and that is not a shortfall: terminalization is one-shot and
 * deliberately does not rewrite the run's failure kind, so `effect-uncertain`
 * stays the platform's own literal over the record (the same reason §2.2's
 * `UncertainPanel` goes on badging it). The glyph is a one-character tree mark
 * handed nothing but a state; whether an operator has since closed the
 * uncertainty is the resolution record's to tell, and §2.2's panel is where the
 * read carries it.
 *
 * Looked up through maps rather than indexed as records, the way `status.ts`
 * does it: `satisfies` still forces every state to be spelled here, and a state
 * this build has never heard of lands in a stated fallback instead of an
 * undefined. Both lookups stay reactive because both are written into `Glyph`'s
 * props rather than run in this body — the glyph is cached display text
 * "refreshed whenever the entity is visited", so a `state` frozen at first
 * render would leave a run that has moved on still wearing the verdict it was
 * first seen with.
 */
const marks: ReadonlyMap<string, string> = new Map(
  Object.entries({
    ok: "✓",
    fail: "✗",
    uncertain: "◌",
    none: "•",
  } satisfies Record<VerdictState, string>),
);

const words: ReadonlyMap<string, string> = new Map(
  Object.entries({
    ok: "verdict ok",
    fail: "verdict fail",
    uncertain: "verdict effect-uncertain",
    none: "no verdict",
  } satisfies Record<VerdictState, string>),
);

/**
 * The state no vocabulary here claims. The wire types run status as a bare
 * string (the STEP-9 RISK on `ExecutionFact`), so a status this console has
 * never heard of can reach the store and come back out as a verdict state that
 * is not one of the four — the type closes that path, the runtime does not.
 * Drawing nothing would leave a dim, unlabelled mark asserting no verdict; the
 * mark says it is a question and the word says which state was not recognized,
 * rendered as it arrived the way `StatusBadge` renders an unheard-of status.
 */
const UNRECOGNIZED_MARK = "?";

function wordFor(state: VerdictState): string {
  const known = words.get(state);
  if (known !== undefined) {
    return known;
  }
  // `runVerdictState` has no default arm, so the absent state arrives here as
  // `undefined` rather than as a word — there is then nothing to quote, and
  // naming a state that was never supplied would be the invention this avoids.
  const literal = typeof state === "string" ? state.trim() : "";
  return literal.length > 0 ? `verdict not recognized: ${literal}` : "verdict not recognized";
}

/**
 * The prop is `state`, matching `verdictTone(state)`, and not `verdict` —
 * `VerdictBar` already takes a `verdict`, and that one is the display line a
 * screen prints, not one of these four words.
 */
export function VerdictGlyph(props: { state: VerdictState }): JSX.Element {
  return (
    <Glyph
      class="verdict-glyph"
      tone={verdictTone(props.state)}
      mark={marks.get(props.state) ?? UNRECOGNIZED_MARK}
      word={wordFor(props.state)}
    />
  );
}
