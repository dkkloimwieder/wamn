/**
 * Inline edit of a QueryTable cell (wamn-8iul.4).
 *
 * A cell of a field the definition's update writes shows its value and an
 * edit button. The editor holds the typed text. Save calls the update with the
 * row's key and revision. A refusal marks the cell and keeps the editor open.
 * A revision conflict marks the row and keeps the typed text. One edit is open
 * at a time, and while it is open no new load runs. After a write, a row
 * whose field neither scopes nor sorts the read stays where it is, as the
 * write left it, and every other write loads again.
 */

import Pencil from "lucide-solid/icons/pencil";
import { createEffect, createSignal, type JSX, on, Show } from "solid-js";

import {
  callOperation,
  type JsonValue,
  type Outcome,
  refusalSentence,
  type Transport,
  writeMember,
  writeSupplied,
} from "@wamn/web-runtime";

import { Button } from "../components/ui/button";
import { Input } from "../components/ui/input";
import type { TableColumn, TableColumnType, TableSort } from "./columns";
import type { QueryTableDefinition } from "./query-definition";
import type { QueryLoad } from "./table-load";

/** What one inline edit came to. */
export type EditResult<TRow> =
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
  type: TableColumnType,
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
      return trimmed === "true" || trimmed === "false" ? { value: trimmed === "true" } : { error: "not true or false" };
    default:
      return { value: type === "text" ? text : trimmed };
  }
}

/** The text an editor starts with for one value. */
export const editText = (value: unknown): string => (value === null || value === undefined ? "" : String(value));

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
          data-slot="table-edit"
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
              <span data-slot="table-cell-refusal" class="truncate text-sm text-destructive">
                {error()}
              </span>
            )}
          </Show>
          <Show when={edit().conflict}>
            {(conflict) => (
              <span data-slot="table-row-conflict" class="truncate text-sm text-destructive">
                {conflict()}
              </span>
            )}
          </Show>
        </div>
      )}
    </Show>
  );
}

/** What one outcome of a write means to its row, when it did not complete. */
export function writeOutcome(outcome: Outcome<unknown>): {
  readonly status: "refused" | "uncertain";
  readonly message: string;
} {
  switch (outcome.status) {
    case "refused":
      return { status: "refused", message: refusalSentence(outcome.code, outcome.text) };
    case "uncertain":
      return { status: "uncertain", message: outcome.reason };
    default:
      return { status: "uncertain", message: "partially completed" };
  }
}

type Member = { readonly [name: string]: unknown };

/** What one outcome of an inline edit means to its cell. The row takes the members the write returned. */
function editResult<TRow>(outcome: Outcome<unknown>, row: TRow): EditResult<TRow> {
  if (outcome.status === "completed") {
    const written = (outcome.value ?? {}) as Member;
    const next = { ...row } as Member & TRow;
    for (const name of Object.keys(next)) {
      if (name in written) {
        (next as { [name: string]: unknown })[name] = written[name];
      }
    }
    return { status: "completed", row: next };
  }
  const result = writeOutcome(outcome);
  return outcome.status === "refused" && outcome.code === "concurrency_conflict"
    ? { ...result, status: "conflict" }
    : result;
}

/** The inline edit of a QueryTable: the one open edit, and the cell of each declared column. */
export function createEdits<TRow extends object>(options: {
  readonly definition: QueryTableDefinition<TRow>;
  readonly load: QueryLoad<TRow>;
  readonly transport: Transport;
  readonly idOf: (row: TRow) => string;
  /** The sort the query holds. An edit of a sorted field loads again. */
  readonly sort: () => readonly TableSort[];
}) {
  const { definition, load } = options;
  const update = definition.update;
  const [edit, setEdit] = createSignal<OpenEdit | null>(null);
  createEffect(on(() => edit() !== null, load.hold, { defer: true }));

  /** Saves the open edit. A refusal or a conflict keeps the editor and its text. */
  async function save() {
    const open = edit();
    const row =
      open === null ? undefined : load.state().rows.find((candidate) => options.idOf(candidate) === open.rowId);
    if (open === null || row === undefined || open.saving || update === undefined) {
      return;
    }
    const type = definition.columns.find((column) => column.field === open.field)!.type;
    const parsed = editedValue(type, open.text);
    if ("error" in parsed) {
      setEdit({ ...open, error: parsed.error });
      return;
    }
    setEdit({ ...open, saving: true });
    const target = update.fields.find((candidate) => candidate.field === open.field)!;
    const member = row as Member;
    let written = writeMember({}, update.keyInput, member[definition.rowId[0]!] as JsonValue);
    if (update.revisionInput !== undefined && update.revisionField !== undefined) {
      written = writeMember(written, update.revisionInput, member[update.revisionField] as JsonValue);
    }
    written = writeSupplied(writeMember(written, target.input, parsed.value as JsonValue), update.supplied);
    const result = editResult(await load.write(() => callOperation(options.transport, update.binding, [written])), row);
    const current = edit();
    if (current === null) {
      return;
    }
    if (result.status === "completed") {
      setEdit(null);
      const moves =
        definition.sortFields.some((sort) => sort.field === open.field) ||
        options.sort().some((sort) => sort.field === open.field) ||
        definition.scopeFilters.includes(open.field as keyof TRow & string);
      if (result.row !== undefined && !moves) {
        load.replaceRow(result.row);
      } else {
        load.reload();
      }
    } else if (result.status === "conflict") {
      setEdit({ ...current, saving: false, error: null, conflict: result.message });
    } else {
      setEdit({ ...current, saving: false, conflict: null, error: result.message });
    }
  }

  /** The cell of a declared column: what it shows, and an editor when the update writes the field. */
  const cell = (declared: TableColumn<TRow>, shown: (row: TRow) => JSX.Element) =>
    update?.fields.some((candidate) => candidate.field === declared.field) !== true
      ? shown
      : (row: TRow, rowId: string) => (
          <EditCell
            label={declared.label}
            rowId={rowId}
            shown={shown(row)}
            edit={edit()?.rowId === rowId && edit()?.field === declared.field ? edit() : null}
            blocked={edit() !== null}
            onOpen={() =>
              edit() === null &&
              setEdit({
                rowId,
                field: declared.field,
                text: editText(row[declared.field]),
                error: null,
                conflict: null,
                saving: false,
              })
            }
            onText={(text) => setEdit((open) => (open === null ? null : { ...open, text }))}
            onSave={() => void save()}
            onDrop={() => setEdit(null)}
          />
        );

  return { open: () => edit() !== null, cell };
}
