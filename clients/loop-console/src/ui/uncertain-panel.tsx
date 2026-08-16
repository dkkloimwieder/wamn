import type { JSX } from "@solidjs/web";
import { For, Show } from "solid-js";

import type { EffectUncertainty } from "../reader/types";
import { KeyValue } from "./key-value";
import { SectionLabel } from "./section-label";
import { EmptyState } from "./states";
import { StatusBadge } from "./status-badge";
import "./tone.css";
import "./uncertain-panel.css";

/**
 * §2.2's uncertain panel: the section that replaces RUN FAILURE when an effect
 * may have escaped, and the screen's dominant element while it stands there.
 *
 * Dominance is bought in §1.2's scale — the platform's meaning at head size in
 * the frame face against 12px labels and 13px cells — not with a second status
 * rule: §1.3 gives that rule to the verdict bar alone and lets nothing else on
 * the screen compete with it. So the panel opens the way every other section
 * opens (§1.4's label on a hairline) and keeps its purple where §1.1 puts it,
 * inside a badge beside the word. The panel *is* the section it stands in and
 * opens its own; a caller must not wrap it in another `SectionLabel`.
 *
 * The panel says two things and invents neither. The platform's meaning is
 * printed sentence by sentence exactly as `EFFECT_UNCERTAIN_MEANING` carries it
 * — this is the one text on the screen a paraphrase would make dangerous. The
 * operator resolution is printed as it reads back: `unresolved` states that no
 * action is *recorded*, which is all the read knows, and a terminalized run
 * shows the whole recorded action.
 *
 * The badge's status word is a literal rather than read data, licensed by the
 * `Run.uncertainty` invariant — present exactly when the run is
 * effect-uncertain, resolved or not. Nothing here retires that state:
 * terminalization is one-shot and deliberately does not rewrite the run's
 * failure kind, so the badge goes on saying `effect-uncertain` over the record.
 */
export function UncertainPanel(props: { uncertainty: EffectUncertainty }): JSX.Element {
  /** The record, or null — `Show`'s callback narrows the union a ternary in JSX cannot. */
  const terminalized = () => {
    const resolution = props.uncertainty.resolution;
    return resolution.state === "operator-terminalized" ? resolution : null;
  };

  return (
    <div class="uncertain-panel" data-tone="uncertain">
      <div class="uncertain-meaning">
        <SectionLabel
          label="effect uncertain"
          trailing={<StatusBadge status="effect-uncertain" tone="uncertain" />}
        />
        {/* the panel's whole subject: an empty read is stated, never left blank */}
        <Show
          when={props.uncertainty.meaning.length > 0}
          fallback={<EmptyState region="effect uncertain" reason="the read carried no meaning" />}
        >
          <For each={props.uncertainty.meaning}>
            {(sentence) => <p class="uncertain-sentence frame">{sentence}</p>}
          </For>
        </Show>
      </div>
      <div class="uncertain-resolution">
        <SectionLabel
          label="operator resolution"
          trailing={
            <span class="uncertain-resolution-state">{props.uncertainty.resolution.state}</span>
          }
        />
        <Show
          when={terminalized()}
          fallback={
            <EmptyState
              region="operator resolution"
              reason="no operator action is recorded for this run"
            />
          }
        >
          {(resolution) => (
            <div class="uncertain-record">
              <KeyValue label="basis">{resolution().basis}</KeyValue>
              <KeyValue label="evidence">{resolution().evidenceRef ?? <NoEvidence />}</KeyValue>
              <KeyValue label="operator">{resolution().principal}</KeyValue>
              <KeyValue label="reason">{resolution().terminalReason}</KeyValue>
              <KeyValue label="when">{resolution().at}</KeyValue>
            </div>
          )}
        </Show>
      </div>
    </div>
  );
}

/**
 * An absent evidence ref is stated, never blanked: `operator-judgment` carries
 * no reference by its nature, and a blank row would read as one that was lost.
 */
function NoEvidence(): JSX.Element {
  return <span class="uncertain-absent frame">none recorded</span>;
}
