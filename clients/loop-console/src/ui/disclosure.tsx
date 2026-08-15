import type { JSX } from "@solidjs/web";
import { Show, createSignal, createUniqueId } from "solid-js";

import "./disclosure.css";

export type DisclosureProps = {
  label: string;
  open?: boolean;
  onToggle?: (next: boolean) => void;
  children: JSX.Element;
};

/**
 * Uncontrolled until `open` is supplied, because step 4's expand-all/collapse-all
 * moves every disclosure at once — which a per-disclosure signal cannot serve.
 */
export function Disclosure(props: DisclosureProps): JSX.Element {
  const panelId = createUniqueId();
  const [ownOpen, setOwnOpen] = createSignal(false);
  const open = () => props.open ?? ownOpen();
  // spelled out: @solidjs/web renders a boolean aria value as a boolean
  // attribute, so `false` would come out as no attribute at all — and a button
  // with no aria-expanded is not a disclosure.
  const expanded = () => (open() ? "true" : "false");

  function toggle(): void {
    const next = !open();
    setOwnOpen(next);
    props.onToggle?.(next);
  }

  return (
    <div class="disclosure">
      <button
        type="button"
        class="disclosure-toggle"
        aria-expanded={expanded()}
        aria-controls={panelId}
        onClick={toggle}
      >
        <span class="disclosure-marker" aria-hidden="true">
          {open() ? "▾" : "▸"}
        </span>
        {props.label}
      </button>
      {/* Closed content is never created, so nothing in it is reachable. */}
      <Show when={open()}>
        <div id={panelId} class="disclosure-panel">
          {props.children}
        </div>
      </Show>
    </div>
  );
}
