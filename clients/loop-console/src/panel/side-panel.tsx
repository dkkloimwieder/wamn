import type { JSX } from "@solidjs/web";
import { For, Show, createEffect, createMemo, createSignal, onCleanup } from "solid-js";

import { focusAnchor, screenContribution } from "../app/screen-actions";
import { toHash, type Route } from "../routing/route";
import { visitedEntries } from "../store/visited";
import { EmptyState } from "../ui/states";
import { VerdictGlyph } from "../ui/verdict-glyph";
import {
  buildTree,
  parentKeys,
  sectionInView,
  visibleRows,
  type TreeNode,
} from "./panel-model";
import { closedKinds, panelCollapsed, setKindOpen, togglePanelCollapsed } from "./panel-state";
import "./side-panel.css";

/**
 * §2.0's side panel: the console's map, drawn from what this browser has
 * visited because there are no list endpoints to draw it from anything else.
 *
 * `panel-model.ts` decides which rows stand and what each one says; this file
 * owns only the interaction — moving, expanding, activating, and marking the
 * section a reader has scrolled to.
 *
 * **No key is bound outside this widget.** §6 step 7 rules that the palette is
 * the console's only keyboard route to navigation and there are no direct keys
 * anywhere else, so the panel adds no document listener at all — not even §2.6's
 * `[`. What lives here is the tree's own `↑↓ ←→ Home End Enter`, which is
 * within-widget list convention (the palette's `↑↓` are there on the same
 * footing) and not a binding a reader has to be taught.
 */

/**
 * How often the relative times are re-read.
 *
 * The store holds the epoch and never the phrase precisely so `2m ago` cannot go
 * stale (`palette/commands.ts`), and the palette re-takes its clock every time it
 * opens. The panel never closes, so it has to take its own — a tree left up for
 * an hour would otherwise still be saying `2m ago` about a visit from an hour
 * back, which is the one thing caching the phrase would have done wrong.
 */
const CLOCK_MS = 30_000;

export function SidePanel(props: { route: Route }): JSX.Element {
  const [now, setNow] = createSignal(Date.now());
  const clock = setInterval(() => setNow(Date.now()), CLOCK_MS);
  onCleanup(() => clearInterval(clock));

  const tree = createMemo(() =>
    buildTree({
      route: props.route,
      visited: visitedEntries(),
      // The mounted screen's live offer, read inside the memo so a read that
      // settles into a new section grows the tree under the reader.
      anchors: screenContribution().anchors,
      closedKinds: closedKinds(),
      collapsed: panelCollapsed(),
      now: now(),
    }),
  );
  const rows = createMemo(() => visibleRows(tree()));
  const parents = createMemo(() => parentKeys(tree()));

  /*
   * Roving focus: exactly one row is in the tab order and the arrows move which
   * one, so the whole tree is a single tab stop.
   *
   * The cursor is held as the row's *key* rather than as a place in the list,
   * for `palette.tsx`'s reason — the list is live (a clock tick, a settled read,
   * a collapsed kind all rebuild it), and a number would quietly come to mean a
   * different row. A key that is no longer in the list resolves to the top: the
   * row a reader was on is gone, and pointing at whatever slid into its place
   * would claim otherwise.
   */
  const [focusedKey, setFocusedKey] = createSignal<string | null>(null);
  const focused = (): string | null => {
    const list = rows();
    if (list.length === 0) {
      return null;
    }
    const key = focusedKey();
    return list.some((row) => row.key === key) ? key : list[0].key;
  };

  /*
   * The row elements, by key. A plain map rather than a query: an entity's key
   * carries an opaque id, and building a selector out of one would be building
   * a parser for ids this console is documented never to read.
   */
  const elements = new Map<string, HTMLElement>();

  function moveTo(key: string | undefined): void {
    if (key === undefined) {
      return;
    }
    setFocusedKey(key);
    // The row is already in the document — moving the cursor never adds or
    // removes one — so focus can be given now rather than waiting for a flush.
    elements.get(key)?.focus();
  }

  function activate(node: TreeNode): void {
    if (node.unavailable !== null) {
      // §2.0's dimmed `structure`: the row says why, and does not pretend to act.
      return;
    }
    if (node.route !== null) {
      // Routes are the state (§2.6); the address bar is how the panel navigates.
      window.location.hash = toHash(node.route);
      return;
    }
    if (node.anchor !== null) {
      // "Anchors, not routes — the URL stays the entity" (§2.0).
      focusAnchor(node.anchor);
      return;
    }
    if (node.kind !== null && node.expanded !== null) {
      setKindOpen(node.kind, node.expanded === false);
    }
  }

  function onKeyDown(event: KeyboardEvent, node: TreeNode): void {
    const list = rows();
    const at = list.findIndex((row) => row.key === node.key);
    switch (event.key) {
      case "ArrowDown":
        event.preventDefault();
        moveTo(list[at + 1]?.key);
        return;
      case "ArrowUp":
        event.preventDefault();
        moveTo(list[at - 1]?.key);
        return;
      case "Home":
        event.preventDefault();
        moveTo(list[0]?.key);
        return;
      case "End":
        event.preventDefault();
        moveTo(list[list.length - 1]?.key);
        return;
      case "ArrowRight":
        event.preventDefault();
        if (node.expanded === false && node.kind !== null) {
          setKindOpen(node.kind, true);
          return;
        }
        moveTo(node.children[0]?.key);
        return;
      case "ArrowLeft":
        event.preventDefault();
        if (node.expanded === true && node.kind !== null) {
          setKindOpen(node.kind, false);
          return;
        }
        /*
         * An expanded row that is not a kind is an *entity*, and §2.0 holds its
         * sections open for exactly as long as it is the active one — there is
         * no collapse to perform. Standing still would leave the key doing
         * nothing on a row where the pattern promises movement, so it does what
         * the pattern does everywhere else it cannot collapse: goes to the parent.
         */
        moveTo(parents().get(node.key));
        return;
      case "Enter":
        event.preventDefault();
        activate(node);
        return;
      default:
        return;
    }
  }

  /*
   * §2.0's "the section in view is marked as you scroll".
   *
   * The decision is `sectionInView`'s and is tested without a browser; this is
   * only the wiring that feeds it. `visible` and `marked` are plain variables
   * because the observer computes the next mark from them — reading that back
   * out of a signal would be reading a write that has not flushed, which is the
   * hazard `store/visited.ts` and the palette both keep naming.
   */
  const [inView, setInView] = createSignal<string | null>(null);
  const anchorIds = createMemo(() =>
    rows()
      .filter((row) => row.anchor !== null && row.unavailable === null)
      .map((row) => row.anchor as string),
  );
  let visible = new Set<string>();
  let marked: string | null = null;
  let observer: IntersectionObserver | null = null;

  function remark(ids: readonly string[]): void {
    const next = sectionInView(ids, [...visible], marked);
    if (next === marked) {
      return;
    }
    marked = next;
    setInView(next);
  }

  createEffect(
    () => anchorIds(),
    (ids) => {
      observer?.disconnect();
      observer = null;
      visible = new Set();
      marked = null;
      setInView(null);
      /*
       * Built behind a capability check rather than assumed: jsdom implements no
       * `IntersectionObserver` at all (measured — see `vitest.config.ts`), so
       * the panel is inert where it is absent instead of throwing there. The
       * proof that the wiring actually marks the right section is therefore a
       * browser proof, and lives in `side-panel.browser.test.tsx`.
       */
      if (ids.length === 0 || typeof IntersectionObserver !== "function") {
        return;
      }
      const next = new IntersectionObserver((entries) => {
        for (const entry of entries) {
          if (entry.isIntersecting) {
            visible.add(entry.target.id);
          } else {
            visible.delete(entry.target.id);
          }
        }
        remark(ids);
      });
      for (const id of ids) {
        const section = document.getElementById(id);
        if (section !== null) {
          next.observe(section);
        }
      }
      observer = next;
    },
  );
  onCleanup(() => observer?.disconnect());

  /** The row's one glyph column, held whether or not this row has a mark in it. */
  const markOf = (node: TreeNode): string => {
    if (node.anchor !== null) {
      return node.anchor === inView() ? "▸" : "";
    }
    // The rail has no room for a mark column at all, so what little it can show
    // is carried by the row's own text — §2.0's `◈` and the kind initials.
    if (panelCollapsed()) {
      return "";
    }
    return node.type === "start" ? "◈" : node.expanded === null ? "" : node.expanded ? "▾" : "▸";
  };

  const renderNodes = (nodes: readonly TreeNode[]): JSX.Element => (
    <For each={nodes}>
      {(node) => (
        <div class="tree-node" role="none">
          <div
            class="tree-row"
            role="treeitem"
            data-type={node.type}
            ref={(element) => elements.set(node.key, element)}
            aria-level={node.level}
            // Spelled out, every one of them: @solidjs/web renders a boolean
            // aria value as an empty string, which is invalid and silent.
            aria-expanded={node.expanded === null ? undefined : node.expanded ? "true" : "false"}
            aria-selected={node.active ? "true" : undefined}
            aria-current={node.anchor !== null && node.anchor === inView() ? "true" : undefined}
            aria-disabled={node.unavailable === null ? undefined : "true"}
            tabindex={focused() === node.key ? 0 : -1}
            title={node.full ?? node.hint ?? undefined}
            onFocus={() => setFocusedKey(node.key)}
            onClick={() => activate(node)}
            onKeyDown={(event) => onKeyDown(event, node)}
          >
            <span class="tree-row-mark" aria-hidden="true">
              {markOf(node)}
            </span>
            <Show when={node.verdict}>
              {(verdict) => <VerdictGlyph state={verdict()} />}
            </Show>
            {/*
             * Hidden from a reader only when the whole text is said below it —
             * a middle-ellipsized id and the rail's initials are for the eye,
             * and neither is what the row is called.
             */}
            <span
              // §1.2's label role for level 1, worn as the class tokens.css
              // declares it: no other file in this package may name a face.
              class={node.type === "kind" ? "tree-row-text label" : "tree-row-text"}
              aria-hidden={node.full === null ? undefined : "true"}
            >
              {node.label}
            </span>
            <Show when={node.full}>
              {(full) => <span class="visually-hidden">{full()}</span>}
            </Show>
            <Show when={node.detail}>
              {(detail) => <span class="tree-row-detail frame">{detail()}</span>}
            </Show>
            <Show when={node.hint}>{(hint) => <span class="visually-hidden">{hint()}</span>}</Show>
            {/* §2.0's empty state, inside the row it is about so a reader hears
                the kind and the absence as one thing. */}
            <Show when={node.emptyNote}>
              {(note) => <EmptyState region={node.label.toLowerCase()} reason={note()} />}
            </Show>
          </div>
          <Show when={node.expanded !== false && node.children.length > 0}>
            <div class="tree-group" role="group">
              {renderNodes(node.children)}
            </div>
          </Show>
        </div>
      )}
    </For>
  );

  return (
    <>
      <div class="nav-tree" role="tree" aria-label="visited entities">
        {renderNodes(tree())}
      </div>
      <div class="panel-footer">
        <button
          type="button"
          class="panel-toggle"
          aria-expanded={panelCollapsed() ? "false" : "true"}
          aria-label={panelCollapsed() ? "expand the side panel" : "collapse the side panel"}
          onClick={togglePanelCollapsed}
        >
          <span aria-hidden="true">{panelCollapsed() ? "»" : "«"}</span>
          <Show when={!panelCollapsed()}>
            <span class="frame">collapse</span>
          </Show>
        </button>
      </div>
    </>
  );
}
