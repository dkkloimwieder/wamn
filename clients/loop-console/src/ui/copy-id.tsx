import type { JSX } from "@solidjs/web";
import { createSignal, onCleanup } from "solid-js";

import { copyText } from "./copy";
import "./copy-id.css";

export type CopyIdProps = {
  id: string;
  /** What the id names — `run`, `report`, `trace`, `test-set` — so the button says what it copies. */
  label: string;
};

/** §2.6's middle-ellipsis: first and last 4. Anything that short is already whole. */
export function ellipsize(id: string): string {
  return id.length > 8 ? `${id.slice(0, 4)}…${id.slice(-4)}` : id;
}

const outcomeWord = { idle: "", copied: "copied", failed: "copy failed" } as const;

/** Long enough to read, short enough that the row settles back on its own. */
const confirmationMs = 1200;

/** §2.6: every id on every screen is a ⧉ copy affordance. */
export function CopyId(props: CopyIdProps): JSX.Element {
  const [outcome, setOutcome] = createSignal<keyof typeof outcomeWord>("idle");
  const shown = () => ellipsize(props.id);
  let settle: ReturnType<typeof setTimeout> | undefined;
  let disposed = false;
  onCleanup(() => {
    disposed = true;
    clearTimeout(settle);
  });

  async function copy(): Promise<void> {
    // the whole id, never the ellipsized display text
    const landed = await copyText(props.id);
    // the clipboard is permission-gated, so this can land after the owner is gone
    if (disposed) {
      return;
    }
    clearTimeout(settle);
    setOutcome(landed ? "copied" : "failed");
    settle = setTimeout(() => setOutcome("idle"), confirmationMs);
  }

  return (
    <span class="copy-id">
      <button
        type="button"
        class="copy-id-button"
        title={shown() === props.id ? undefined : props.id}
        aria-label={`copy ${props.label} id ${props.id}`}
        onClick={() => void copy()}
      >
        {shown()}
        <span class="copy-id-mark" aria-hidden="true">
          ⧉
        </span>
      </button>
      <span class="copy-id-outcome frame" aria-live="polite">
        {outcomeWord[outcome()]}
      </span>
    </span>
  );
}
