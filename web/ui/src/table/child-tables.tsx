/**
 * Child tables of a QueryTable row (wamn-8iul.6).
 *
 * A child table is its own table definition, rendered in the detail of a
 * parent row. The parent row's id is the value of the child's scope filter,
 * and the caller renders the child from that value alone, so the child reads
 * nothing else of its parent.
 *
 * A child mounts, and so loads, when its row first opens. Closing keeps it:
 * the area lives in its own root, which the table keeps by row id while the
 * scope value is unchanged. It ends when a load drops the row or the table
 * goes. Which rows are open is the QueryTable's, so it holds whether or not a
 * set table shows the rows.
 */

import { ChevronDown, ChevronRight } from "lucide-solid";
import { createRoot, createSignal, For, getOwner, type JSX, onCleanup, runWithOwner, Show } from "solid-js";

import { Button } from "../components/ui/button";
import type { BuiltColumn } from "./columns";

/** One child table of a row: its label, and the table for one scope value. */
export interface ChildTable {
  readonly label: string;
  /** Renders the child table scoped to the parent row's id. */
  readonly render: (scopeValue: string) => JSX.Element;
}

/** The id of the column that opens a row to its child tables. */
export const EXPAND_COLUMN = "rowExpand";

/** The height of one child table's box, in pixels. The `h-96` below sets it. */
export const CHILD_HEIGHT = 384;

/**
 * The child areas of a table's rows: which rows are open, and the area of each
 * row, kept by row id while its scope value holds. `area` returns the kept
 * area, or makes it on the first open.
 */
export function createChildAreas(children: () => readonly ChildTable[]) {
  const owner = getOwner();
  const kept = new Map<string, { readonly value: string; readonly nodes: JSX.Element; readonly dispose: () => void }>();
  const [open, setOpen] = createSignal<ReadonlySet<string>>(new Set());

  const drop = (rowId: string) => {
    kept.get(rowId)?.dispose();
    kept.delete(rowId);
  };

  const area = (rowId: string, value: string): JSX.Element => {
    const current = kept.get(rowId);
    if (current !== undefined && current.value === value) {
      return current.nodes;
    }
    drop(rowId);
    // The area's own root outlives a close, and the table's owner ends it.
    const made = runWithOwner(owner, () =>
      createRoot((dispose) => ({
        value,
        dispose,
        nodes: (
          <div data-slot="table-children" class="flex flex-col gap-4 py-2">
            <For each={children()}>
              {(child) => (
                <section data-slot="table-child" class="flex flex-col gap-2">
                  <h3 class="text-sm font-medium">{child.label}</h3>
                  <div class="h-96 min-h-0">{child.render(value)}</div>
                </section>
              )}
            </For>
          </div>
        ),
      })),
    )!;
    kept.set(rowId, made);
    return made.nodes;
  };

  const toggle = (rowId: string) =>
    setOpen((current) => {
      const next = new Set(current);
      if (!next.delete(rowId)) {
        next.add(rowId);
      }
      return next;
    });

  /** Ends the area of every row that is no longer loaded. */
  const keepOnly = (rowIds: ReadonlySet<string>) => {
    for (const rowId of [...kept.keys()]) {
      if (!rowIds.has(rowId)) {
        drop(rowId);
      }
    }
  };

  onCleanup(() => {
    for (const rowId of [...kept.keys()]) {
      drop(rowId);
    }
  });

  return { open, toggle, area, keepOnly };
}

export type ChildAreas = ReturnType<typeof createChildAreas>;

/** The column that opens a data row to its child tables. The grid shows them under the row. */
export function expandColumn<TRow extends object>(areas: ChildAreas): BuiltColumn<TRow> {
  return {
    id: EXPAND_COLUMN,
    size: 48,
    header: () => "",
    cell: (_row, rowId) => {
      const opened = () => areas.open().has(rowId);
      return (
        <Button
          type="button"
          variant="ghost"
          size="icon-xs"
          aria-label={`${opened() ? "collapse" : "expand"} ${rowId}`}
          aria-expanded={opened()}
          onClick={() => areas.toggle(rowId)}
        >
          <Show when={opened()} fallback={<ChevronRight aria-hidden="true" />}>
            <ChevronDown aria-hidden="true" />
          </Show>
        </Button>
      );
    },
  };
}
