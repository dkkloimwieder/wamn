/**
 * Child tables of the DataTable (wamn-8iul.6).
 *
 * A child table is its own table definition, rendered in the expanded area of
 * a parent row. The parent row's id is the value of the child's scope filter,
 * and the caller renders the child from that value alone, so the child reads
 * nothing else of its parent.
 *
 * A child mounts, and so loads, when its row first expands. Collapsing keeps
 * it: the area lives in its own root, which the table keeps by row id while
 * the scope value is unchanged. It ends when a load drops the row or the
 * table goes.
 */

import type { ColumnDef } from "@tanstack/solid-table";
import { ChevronDown, ChevronRight } from "lucide-solid";
import { createRoot, For, getOwner, type JSX, onCleanup, runWithOwner, Show } from "solid-js";

import { Button } from "../components/ui/button";
import type { DataTableFeatures } from "./data-table";

/** One child table of a row: its label, and the table for one scope value. */
export interface DataTableChild {
  readonly label: string;
  /** Renders the child table scoped to the parent row's id. */
  readonly render: (scopeValue: string) => JSX.Element;
}

/** The id of the column that expands a row to its child tables. */
export const EXPAND_COLUMN = "rowExpand";

/** The height of one child table's box, in pixels. The `h-96` below sets it. */
export const CHILD_HEIGHT = 384;

/**
 * Keeps the expanded area of each row, by row id, while its scope value holds.
 * `area` returns the kept area, or makes it on the first expand.
 */
export function createChildAreas(children: () => readonly DataTableChild[]) {
  const owner = getOwner();
  const kept = new Map<string, { readonly value: string; readonly nodes: JSX.Element; readonly dispose: () => void }>();

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
    // The area's own root outlives a collapse, and the table's owner ends it.
    const made = runWithOwner(owner, () =>
      createRoot((dispose) => ({
        value,
        dispose,
        nodes: (
          <div data-slot="data-table-children" class="flex flex-col gap-4 py-2">
            <For each={children()}>
              {(child) => (
                <section data-slot="data-table-child" class="flex flex-col gap-2">
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

  return { area, keepOnly };
}

/** The column that expands a data row to its child tables. */
export function expandColumn<TRow extends object>(
  area: (row: TRow) => JSX.Element,
): ColumnDef<DataTableFeatures, TRow> {
  return {
    id: EXPAND_COLUMN,
    header: "",
    cell: (context) => (
      <Show when={!context.row.getIsGrouped()}>
        <Button
          type="button"
          variant="ghost"
          size="icon-xs"
          aria-label={`${context.row.getIsExpanded() ? "collapse" : "expand"} ${context.row.id}`}
          aria-expanded={context.row.getIsExpanded()}
          onClick={() => context.row.toggleExpanded()}
        >
          <Show when={context.row.getIsExpanded()} fallback={<ChevronRight aria-hidden="true" />}>
            <ChevronDown aria-hidden="true" />
          </Show>
        </Button>
      </Show>
    ),
    // The grid renders this under an expanded data row. A group row expands
    // to its rows instead, and shows no area.
    meta: { expandedContent: area },
    size: 48,
    enableSorting: false,
    enableColumnFilter: false,
    enableGlobalFilter: false,
    enableGrouping: false,
    enableHiding: false,
    enablePinning: false,
    enableResizing: false,
  };
}
