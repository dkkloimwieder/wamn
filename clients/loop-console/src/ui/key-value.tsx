import type { JSX } from "@solidjs/web";

import "./key-value.css";

/**
 * The pair §2.2's failure panel is built from: kind · at · detail.
 *
 * One list per pair, rather than `dt`/`dd` inside a list the caller supplies:
 * the association is what makes the value readable out of context, so the
 * primitive owns it instead of trusting whatever element it is dropped into.
 *
 * The key deliberately does not wear the `.label` role. That role is §1.4's
 * section divider — the 12px uppercase rule-with-label — and every wireframe
 * prints it uppercase; a pair key is drawn lowercase in an aligned data-face
 * column (`kind        retry-exhausted`), which uppercasing would break.
 */
export function KeyValue(props: { label: string; children: JSX.Element }): JSX.Element {
  return (
    <dl class="key-value">
      <dt class="key-value-label">{props.label}</dt>
      <dd class="key-value-value">{props.children}</dd>
    </dl>
  );
}
