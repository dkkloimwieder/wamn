import type { JSX } from "@solidjs/web";
import { Show } from "solid-js";

import "./refresh-control.css";

export type RefreshControlProps = {
  refreshedAt: Date | null;
  onRefresh: () => void;
  busy?: boolean;
};

/** §2.6's `refreshed 12:04:31 ↻` — the one control that re-issues a screen's read. */
export function RefreshControl(props: RefreshControlProps): JSX.Element {
  const busy = () => props.busy === true;
  // local wall clock; toLocaleTimeString's format is the environment's, not ours
  const at = () => props.refreshedAt?.toTimeString().slice(0, 8) ?? null;

  return (
    <>
      <button
        type="button"
        class="refresh-control"
        disabled={busy()}
        onClick={() => props.onRefresh()}
      >
        <span class="visually-hidden">refresh this screen</span>
        <Show when={at()}>{(time) => <span>refreshed {time()}</span>}</Show>
        <span aria-hidden="true">↻</span>
      </button>
      {/* a disabled button cannot be focused, so the busy state is announced beside it */}
      <span class="visually-hidden" aria-live="polite">
        {busy() ? "refreshing" : ""}
      </span>
    </>
  );
}
