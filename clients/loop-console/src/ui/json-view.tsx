import type { JSX } from "@solidjs/web";
import { For, Show, createMemo, createSignal, createUniqueId, onCleanup } from "solid-js";

import type { JsonValue } from "../reader/types";
import { copyText } from "./copy";
import "./json-view.css";

/**
 * §2.2's captured output and failure raw, and §2.3's terminal-respond bodies:
 * JSON as a collapsible tree in the data face.
 *
 * The expander carries nothing but its glyph. A `<button>` does not inherit a
 * font family and no file but `tokens.css` may declare one, so every character
 * that has to wear the data face sits beside the button, not inside it.
 */

type Child = { readonly label: string; readonly path: string; readonly value: JsonValue };

/** Null for anything with nothing to open: every scalar, and empty containers. */
function children(value: JsonValue, path: string): readonly Child[] | null {
  if (typeof value !== "object" || value === null) {
    return null;
  }
  if (Array.isArray(value)) {
    return value.length === 0
      ? null
      : value.map((item, index) => ({
          label: String(index),
          path: `${path}[${index}]`,
          value: item,
        }));
  }
  const fields = Object.entries(value);
  return fields.length === 0
    ? null
    : fields.map(([key, item]) => ({
        label: `${JSON.stringify(key)}:`,
        path: `${path}.${key}`,
        value: item,
      }));
}

/** A value with nothing to open, spelled as JSON: quoted strings, `{}`, `[]`. */
function inline(value: JsonValue): string {
  if (typeof value === "object" && value !== null) {
    return Array.isArray(value) ? "[]" : "{}";
  }
  return JSON.stringify(value);
}

/** The bracket pair, and what a collapsed row says in place of its contents. */
function shape(
  value: JsonValue,
  count: number,
): { readonly open: string; readonly close: string; readonly summary: string } {
  return Array.isArray(value)
    ? { open: "[", close: "]", summary: `[ ${count} ${count === 1 ? "item" : "items"} ]` }
    : { open: "{", close: "}", summary: `{ ${count} ${count === 1 ? "key" : "keys"} }` };
}

function JsonNode(props: {
  value: JsonValue;
  /** The key or index this value sits under; empty at the root. */
  label: string;
  /** Path from the view's subject, which is what the expander's name says it opens. */
  path: string;
  depth: number;
  collapseDepth: number;
}): JSX.Element {
  const panelId = createUniqueId();
  const kids = createMemo(() => children(props.value, props.path));
  const brackets = createMemo(() => shape(props.value, kids()?.length ?? 0));
  // Seeded once, not derived: a view is mounted per value. Swapping `value` on
  // a live view rebuilds every child from `kids` but leaves this root as it was.
  const [open, setOpen] = createSignal(props.depth < props.collapseDepth);

  return (
    <li class="json-row">
      <Show
        when={kids() !== null}
        fallback={
          <div>
            <span class="json-mark" />
            <span class="json-label">{props.label}</span>{" "}
            <span>{inline(props.value)}</span>
          </div>
        }
      >
        <div>
          <button
            type="button"
            class="json-mark json-expander"
            // spelled out: a `false` here is an attribute Solid would drop
            aria-expanded={open() ? "true" : "false"}
            // only while the children exist; a closed row controls nothing
            aria-controls={open() ? panelId : undefined}
            aria-label={`${open() ? "collapse" : "expand"} ${props.path}`}
            onClick={() => setOpen(!open())}
          >
            {open() ? "▾" : "▸"}
          </button>
          <span class="json-label">{props.label}</span>{" "}
          <span class="json-brace">{open() ? brackets().open : brackets().summary}</span>
        </div>
        <Show when={open()}>
          <ul id={panelId} class="json-children" role="list">
            <For each={kids() ?? []}>
              {(child) => (
                <JsonNode
                  value={child.value}
                  label={child.label}
                  path={child.path}
                  depth={props.depth + 1}
                  collapseDepth={props.collapseDepth}
                />
              )}
            </For>
          </ul>
          <div>
            <span class="json-mark" />
            <span class="json-brace">{brackets().close}</span>
          </div>
        </Show>
      </Show>
    </li>
  );
}

/** CopyId's constant: long enough to read, short enough that the row settles itself. */
const confirmationMs = 1200;

export function JsonView(props: {
  value: JsonValue;
  /**
   * What this JSON is. §2.2 puts several views on one screen — the failure raw
   * and a captured output per disclosed fact — so every name here has to say
   * which evidence it acts on, not merely that it is JSON.
   */
  subject: string;
  collapseDepth?: number;
}): JSX.Element {
  const [copied, setCopied] = createSignal<boolean | null>(null);
  const result = () => (copied() === null ? "" : copied() === true ? "copied" : "copy failed");
  let settle: ReturnType<typeof setTimeout> | undefined;
  onCleanup(() => clearTimeout(settle));

  async function copy(): Promise<void> {
    const landed = await copyText(JSON.stringify(props.value, null, 2));
    clearTimeout(settle);
    setCopied(landed);
    settle = setTimeout(() => setCopied(null), confirmationMs);
  }

  return (
    <div class="json-view">
      <div class="json-actions">
        <button
          type="button"
          class="json-copy frame"
          aria-label={`copy ${props.subject}`}
          onClick={() => void copy()}
        >
          copy ⧉
        </button>
        <span class="json-copy-result frame" role="status">
          {result()}
        </span>
      </div>
      <ul class="json-tree" role="list">
        <JsonNode
          value={props.value}
          label=""
          path={props.subject}
          depth={0}
          // The root and its immediate containers open, so a small captured
          // output reads whole without a click and a deep one stays scannable.
          collapseDepth={props.collapseDepth ?? 2}
        />
      </ul>
    </div>
  );
}
