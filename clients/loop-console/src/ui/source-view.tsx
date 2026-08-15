import type { JSX } from "@solidjs/web";
import { For, Show, createMemo, createSignal, onCleanup } from "solid-js";

import { copyText } from "./copy";
import "./source-view.css";
// The error line paints with var(--tone), which tone.css is what defines.
import "./tone.css";

/**
 * §2.4's source tab: the exact stored bytes, numbered and unwrapped, because
 * the bytes are the draft.
 */

/**
 * A trailing newline terminates the last line rather than starting another, so
 * a draft ending in `\n` numbers to N and not to a phantom empty N+1. Only the
 * gutter is affected: copy takes the `text` prop itself, never these lines back
 * together, so the bytes cannot follow this decision.
 */
function lines(text: string): readonly string[] {
  const split = text.split("\n");
  return split.length > 1 && split[split.length - 1] === "" ? split.slice(0, -1) : split;
}

/** CopyId's constant: long enough to read, short enough that the row settles itself. */
const confirmationMs = 1200;

export function SourceView(props: { text: string; errorLine?: number | null }): JSX.Element {
  const rows = createMemo(() => lines(props.text));
  const [copied, setCopied] = createSignal<boolean | null>(null);
  const result = () => (copied() === null ? "" : copied() === true ? "copied" : "copy failed");
  let settle: ReturnType<typeof setTimeout> | undefined;
  onCleanup(() => clearTimeout(settle));

  /**
   * §2.4 never tints a guessed line, and a line this source does not have is
   * the same guess: only a row that carries the number can match it, so a null
   * — `DraftParse.line` is null on several real engine shapes — and a number
   * past the last row both tint nothing, without a range test to keep true.
   */
  const tinted = () => props.errorLine ?? null;

  async function copy(): Promise<void> {
    // the stored bytes, never the rendered lines reassembled
    const landed = await copyText(props.text);
    clearTimeout(settle);
    setCopied(landed);
    settle = setTimeout(() => setCopied(null), confirmationMs);
  }

  return (
    <div class="source-view">
      <ol class="source-lines" role="list">
        <For each={rows()}>
          {(line, index) => (
            <li class="source-line" data-tone={index() + 1 === tinted() ? "fail" : undefined}>
              <span class="source-number" aria-hidden="true">
                {index() + 1}
              </span>
              <Show when={index() + 1 === tinted()}>
                <span class="visually-hidden">parse error on line {index() + 1}:</span>
              </Show>
              <span class="source-text">{line}</span>
            </li>
          )}
        </For>
      </ol>
      {/* §2.4's wireframe trails the last line with it, at the block's right */}
      <div class="source-actions">
        <span class="source-copy-result frame" role="status">
          {result()}
        </span>
        <button
          type="button"
          class="source-copy frame"
          aria-label="copy the exact stored source"
          onClick={() => void copy()}
        >
          copy exact ⧉
        </button>
      </div>
    </div>
  );
}
