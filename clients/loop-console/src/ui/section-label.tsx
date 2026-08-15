import type { JSX } from "@solidjs/web";

import "./section-label.css";

/**
 * §1.4: the label *is* the divider — the hairline runs out of the heading to the
 * column's edge and nothing boxes the section it opens.
 *
 * The trailing slot (§2.2's `14 facts, all`, §2.6's refresh control) sits beside
 * the heading rather than inside it, so a count or a control never becomes part
 * of the heading's accessible name.
 */
export function SectionLabel(props: { label: string; trailing?: JSX.Element }): JSX.Element {
  return (
    <div class="section-label">
      <h2 class="section-label-text label">{props.label}</h2>
      <span class="section-rule" aria-hidden="true" />
      {props.trailing}
    </div>
  );
}
