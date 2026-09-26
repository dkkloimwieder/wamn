/**
 * Inline edit of the DataTable (wamn-8iul.4).
 *
 * A cell of an editable field shows its value and an edit button. The editor
 * holds the typed text. Save hands the value to the caller, which calls the
 * update with the row's revision. A refusal marks the cell and keeps the
 * editor open. A revision conflict marks the row and keeps the typed text.
 * One edit is open at a time, and while it is open no new load runs.
 */

import { Pencil } from "lucide-solid";
import { type JSX, Show } from "solid-js";

import { Button } from "../components/ui/button";
import { Input } from "../components/ui/input";
import type { DataTableColumnType } from "./data-table";

/** What one inline edit came to. */
export type DataTableEditResult<TRow> =
  | {
      readonly status: "completed";
      /** The row as the write left it, when the caller knows it. */
      readonly row?: TRow | undefined;
    }
  | { readonly status: "refused" | "conflict" | "uncertain"; readonly message: string };

/** The open edit: one cell, its typed text, and the marks of its last save. */
export interface OpenEdit {
  readonly rowId: string;
  readonly field: string;
  readonly text: string;
  /** Why the last save of this cell was refused, or null. */
  readonly error: string | null;
  /** The revision conflict of the last save, which marks the row, or null. */
  readonly conflict: string | null;
  readonly saving: boolean;
}

/**
 * The value that typed text stands for, in the column's type, or an error.
 * An empty text is null, except in a text column.
 */
export function editedValue(
  type: DataTableColumnType,
  text: string,
): { readonly value: unknown } | { readonly error: string } {
  const trimmed = text.trim();
  if (trimmed === "" && type !== "text") {
    return { value: null };
  }
  switch (type) {
    case "int32":
      return /^-?\d+$/.test(trimmed) ? { value: Number(trimmed) } : { error: "not a whole number" };
    case "int64":
      return /^-?\d+$/.test(trimmed) ? { value: trimmed } : { error: "not a whole number" };
    case "float64":
      return Number.isFinite(Number(trimmed)) ? { value: Number(trimmed) } : { error: "not a number" };
    case "numeric":
      return /^-?\d+(\.\d+)?$/.test(trimmed) ? { value: trimmed } : { error: "not a number" };
    case "boolean":
      return trimmed === "true" || trimmed === "false"
        ? { value: trimmed === "true" }
        : { error: "not true or false" };
    default:
      return { value: type === "text" ? text : trimmed };
  }
}

/** The text an editor starts with for one value. */
export const editText = (value: unknown): string =>
  value === null || value === undefined ? "" : String(value);

export function EditCell(props: {
  readonly label: string;
  readonly rowId: string;
  /** What the cell shows when no edit is open. */
  readonly shown: JSX.Element;
  /** This cell's edit, when it is the open one. */
  readonly edit: OpenEdit | null;
  /** True while another cell's edit is open. */
  readonly blocked: boolean;
  readonly onOpen: () => void;
  readonly onText: (text: string) => void;
  readonly onSave: () => void;
  readonly onDrop: () => void;
}): JSX.Element {
  return (
    <Show
      when={props.edit}
      fallback={
        <div class="group flex items-center gap-1">
          <span class="truncate">{props.shown}</span>
          <Button
            type="button"
            variant="ghost"
            size="icon-xs"
            aria-label={`edit ${props.label} ${props.rowId}`}
            disabled={props.blocked}
            onClick={() => props.onOpen()}
          >
            <Pencil aria-hidden="true" />
          </Button>
        </div>
      }
    >
      {(edit) => (
        <div
          data-slot="data-table-edit"
          data-invalid={edit().error === null ? undefined : "true"}
          class="flex items-center gap-1"
        >
          <Input
            class="h-8 min-w-24"
            aria-label={`${props.label} ${props.rowId}`}
            aria-invalid={edit().error === null ? undefined : "true"}
            value={edit().text}
            disabled={edit().saving}
            onInput={(event) => props.onText(event.currentTarget.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter") {
                props.onSave();
              } else if (event.key === "Escape") {
                props.onDrop();
              }
            }}
          />
          <Button type="button" size="sm" disabled={edit().saving} onClick={() => props.onSave()}>
            save
          </Button>
          <Button type="button" variant="ghost" size="sm" disabled={edit().saving} onClick={() => props.onDrop()}>
            drop
          </Button>
          <Show when={edit().error}>
            {(error) => (
              <span data-slot="data-table-cell-refusal" class="truncate text-sm text-destructive">
                {error()}
              </span>
            )}
          </Show>
          <Show when={edit().conflict}>
            {(conflict) => (
              <span data-slot="data-table-row-conflict" class="truncate text-sm text-destructive">
                {conflict()}
              </span>
            )}
          </Show>
        </div>
      )}
    </Show>
  );
}
