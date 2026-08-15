import type { JSX } from "@solidjs/web";

import type { Tone } from "./status";
import "./status-badge.css";
import "./tone.css";

/**
 * §1.1's rule made concrete: the word is what carries the status, the colour
 * only reinforces it. The word is rendered exactly as it arrived, so a status
 * this console has never heard of is still readable — `runTone` / `nodeRunTone`
 * land it in `neutral` rather than reclassifying it.
 */
export function StatusBadge(props: { status: string; tone: Tone }): JSX.Element {
  return (
    <span class="status-badge" data-tone={props.tone}>
      {props.status}
    </span>
  );
}
