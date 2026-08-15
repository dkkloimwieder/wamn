import type { JSX } from "@solidjs/web";

import "./states.css";

/**
 * The region-reasoned pair (§3): both name the region they stand in, so a
 * reader always knows *what* is loading or empty. Neither invents a value in
 * place of the one that is missing.
 */

/**
 * §1.4 allows no motion here, so there is no spinner to watch: the region's
 * name is the whole affordance. It carries both announcements the floor allows —
 * `aria-busy` states that the region is mid-read whether or not the block is
 * ever announced, and `role="status"` speaks it for the reads that finish while
 * the block is already mounted.
 */
export function LoadingBlock(props: { region: string }): JSX.Element {
  return (
    <p class="loading-block frame" role="status" aria-busy="true">
      loading {props.region}…
    </p>
  );
}

/**
 * §2.2's `capture was off for this run` and §2.0's `nothing visited yet`, both
 * printed as the spec spells them. The region names what is empty for a reader
 * who reached this slot without its section label in view, and is spoken rather
 * than drawn so neither wireframe has to say its own name twice.
 */
export function EmptyState(props: { region: string; reason: string }): JSX.Element {
  return (
    <p class="empty-state frame">
      <span class="visually-hidden">{props.region}: </span>
      {props.reason}
    </p>
  );
}
