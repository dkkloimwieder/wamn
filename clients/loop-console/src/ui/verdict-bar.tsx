import { Show } from "solid-js";
import type { JSX } from "@solidjs/web";

import type { Tone } from "./status";
import "./tone.css";
import "./verdict-bar.css";

/**
 * §1.3, the design's thesis: the answer before the eye moves, once per entity
 * screen — so the verdict is the screen's heading, and every section below it is
 * an h2 under this one.
 *
 * The rule row says the tone twice, in colour and in the tone's own name, which
 * is why the name is not a second prop: the bar states its state itself, and can
 * never be given a word that disagrees with its colour.
 */
export function VerdictBar(props: {
  tone: Tone;
  verdict: string;
  identity?: JSX.Element;
}): JSX.Element {
  return (
    <div class="verdict-bar" data-tone={props.tone}>
      <div class="verdict-rule">{props.tone}</div>
      <h1 class="verdict">{props.verdict}</h1>
      {/* the callback form reads the slot once: naming the prop twice would build
          the caller's CopyIds a second time and drop that copy off the document */}
      <Show when={props.identity}>
        {(identity) => <div class="verdict-identity">{identity()}</div>}
      </Show>
    </div>
  );
}
