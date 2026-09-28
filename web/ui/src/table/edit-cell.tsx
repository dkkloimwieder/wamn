/**
 * Row edit of a QueryTable (wamn-8iul.4, wamn-v50u).
 *
 * A table whose definition has an update leads each row with an edit button.
 * It opens the row: each cell of a field the update writes shows an editor,
 * and a field that names a record shows a select over the list of its
 * records. One save sends one update with the row's key and revision and the
 * fields the operator changed. A refusal marks those cells and keeps the row
 * open, and a revision conflict marks the row and keeps the typed values. One
 * row is open at a time, and while it is open no new load runs. After a write,
 * a row whose changed fields neither scope nor sort the read stays where it
 * is, as the write left it, and every other write loads again.
 */

import { createEffect, createSignal, type JSX, on, onCleanup, Show } from "solid-js";

import {
  afterWrites,
  appendPage,
  callOperation,
  emptyPage,
  firstPage,
  hasNextPage,
  type JsonValue,
  type Outcome,
  type PageState,
  refusalSentence,
  type Transport,
  writeMember,
  writeSupplied,
} from "@wamn/web-runtime";

import { Button } from "../components/ui/button";
import { Input } from "../components/ui/input";
import { RecordSelect } from "../record-select";
import type { BuiltColumn, TableColumn, TableColumnType, TableSort } from "./columns";
import type { QueryTableChoices, QueryTableDefinition } from "./query-definition";
import type { QueryLoad } from "./table-load";

/** What one row edit came to. */
export type EditResult<TRow> =
  | {
      readonly status: "completed";
      /** The row as the write left it, when the caller knows it. */
      readonly row?: TRow | undefined;
    }
  | { readonly status: "refused" | "conflict" | "uncertain"; readonly message: string };

/** The open row: its typed text by field, and the marks of its last save. */
interface OpenRow {
  readonly rowId: string;
  readonly texts: Readonly<Record<string, string>>;
  /** Each field whose text does not fit its column, and why. */
  readonly errors: Readonly<Record<string, string>>;
  /** Why the last save was refused, or null. It marks the fields the save changed. */
  readonly refusal: { readonly message: string; readonly fields: readonly string[] } | null;
  /** The revision conflict of the last save, which marks the row, or null. */
  readonly conflict: string | null;
  readonly saving: boolean;
}

/** The id of the column that leads each row with its edit and save. */
export const EDIT_COLUMN = "rowEdit";

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

/** What one outcome of a row edit means to its row. The row takes the members the write returned. */
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

/**
 * A select over the records a reference field can name. It reads the first
 * page when it opens, a search when the list declares one, and the next page
 * when the operator asks. `readRow` reads the one record the stored value
 * names when no page returned it.
 */
function ChoiceEditor(props: {
  readonly label: string;
  readonly choices: QueryTableChoices;
  readonly transport: Transport;
  readonly value: string;
  readonly invalid: boolean;
  readonly onChange: (value: string) => void;
  readonly readRow?: ((value: string) => Promise<Member | null>) | undefined;
}): JSX.Element {
  const choices = props.choices;
  const [page, setPage] = createSignal<PageState<Member>>(emptyPage<Member>());
  let search = "";
  const read = async (cursor: string | null) => {
    let request: object = {};
    if (cursor !== null && choices.cursorInput !== undefined) {
      request = writeMember(request, choices.cursorInput, cursor);
    }
    if (search !== "" && choices.searchInput !== undefined) {
      request = writeMember(request, choices.searchInput, search);
    }
    const outcome = await callOperation<Member>(props.transport, choices.read, [request]);
    if (outcome.status !== "completed") {
      return;
    }
    const rows = (outcome.value[choices.rows] ?? []) as Member[];
    const next = (outcome.value["nextCursor"] ?? null) as string | null;
    setPage(cursor === null ? firstPage(rows, next) : appendPage(page(), rows, next));
  };
  void read(null);
  onCleanup(afterWrites(props.transport, () => void read(null)));
  return (
    <div class="min-w-48">
      <RecordSelect<Member>
        label={props.label}
        hiddenLabel
        options={page().rows}
        optionValue={(row) => editText(row[choices.keyField])}
        optionLabel={(row) => editText(row[choices.displayField])}
        value={props.value === "" ? null : props.value}
        onChange={(value) => props.onChange(value ?? "")}
        onSearch={
          choices.searchInput === undefined
            ? undefined
            : (text) => {
                search = text;
                void read(null);
              }
        }
        hasNextPage={hasNextPage(page())}
        onNextPage={() => void read(page().cursor)}
        readRow={props.readRow}
        error={props.invalid ? "" : null}
      />
    </div>
  );
}

/** The row edit of a QueryTable: the one open row, its leading column, and the cell of each declared column. */
export function createEdits<TRow extends object>(options: {
  readonly definition: QueryTableDefinition<TRow>;
  readonly load: QueryLoad<TRow>;
  readonly transport: Transport;
  readonly idOf: (row: TRow) => string;
  /** The sort the query holds. An edit of a sorted field loads again. */
  readonly sort: () => readonly TableSort[];
}) {
  const { definition, load, transport } = options;
  const update = definition.update;
  const [open, setOpen] = createSignal<OpenRow | null>(null);
  createEffect(on(() => open() !== null, load.hold, { defer: true }));

  const editable = (field: string) => update?.fields.find((candidate) => candidate.field === field);
  const typeOf = (field: string) => definition.columns.find((column) => column.field === field)!.type;
  const rowOf = (rowId: string) => load.state().rows.find((candidate) => options.idOf(candidate) === rowId);

  /** The record a reference column's stored value names, through the column's record read. */
  const readRow = (field: string, keyField: string, displayField: string) => {
    const column = definition.columns.find((candidate) => candidate.field === field);
    const recordRead = column?.recordRead;
    const shown = column?.displayField;
    if (recordRead === undefined || shown === undefined) {
      return undefined;
    }
    return async (key: string): Promise<Member | null> => {
      const outcome = await callOperation<Member>(transport, recordRead.read, [
        writeMember({}, recordRead.keyInput, key),
      ]);
      return outcome.status === "completed" ? { [keyField]: key, [displayField]: outcome.value[shown] } : null;
    };
  };

  function start(row: TRow, rowId: string) {
    if (open() !== null || update === undefined) {
      return;
    }
    const texts = Object.fromEntries(
      update.fields.map((field) => [field.field, editText((row as Member)[field.field])]),
    );
    setOpen({ rowId, texts, errors: {}, refusal: null, conflict: null, saving: false });
  }

  /** Saves the open row. A refusal or a conflict keeps the row open and its text. */
  async function save() {
    const current = open();
    const row = current === null ? undefined : rowOf(current.rowId);
    if (current === null || row === undefined || current.saving || update === undefined) {
      return;
    }
    const member = row as Member;
    const changed = update.fields.filter((field) => current.texts[field.field] !== editText(member[field.field]));
    const parsed = changed.map(
      (field) => [field, editedValue(typeOf(field.field), current.texts[field.field]!)] as const,
    );
    const errors = Object.fromEntries(
      parsed.flatMap(([field, value]) => ("error" in value ? [[field.field, value.error]] : [])),
    );
    if (Object.keys(errors).length > 0) {
      setOpen({ ...current, errors });
      return;
    }
    if (changed.length === 0) {
      setOpen(null);
      return;
    }
    setOpen({ ...current, errors: {}, saving: true });
    let written = writeMember({}, update.keyInput, member[definition.rowId[0]!] as JsonValue);
    if (update.revisionInput !== undefined && update.revisionField !== undefined) {
      written = writeMember(written, update.revisionInput, member[update.revisionField] as JsonValue);
    }
    for (const [field, value] of parsed) {
      written = writeMember(written, field.input, (value as { value: unknown }).value as JsonValue);
    }
    written = writeSupplied(written, update.supplied);
    const result = editResult(await load.write(() => callOperation(transport, update.binding, [written])), row);
    const after = open();
    if (after === null) {
      return;
    }
    if (result.status === "completed") {
      setOpen(null);
      const moves = changed.some(
        ({ field }) =>
          definition.sortFields.some((sort) => sort.field === field) ||
          options.sort().some((sort) => sort.field === field) ||
          definition.scopeFilters.includes(field as keyof TRow & string),
      );
      if (result.row !== undefined && !moves) {
        load.replaceRow(result.row);
      } else {
        load.reload();
      }
    } else if (result.status === "conflict") {
      setOpen({ ...after, saving: false, refusal: null, conflict: result.message });
    } else {
      setOpen({
        ...after,
        saving: false,
        conflict: null,
        refusal: { message: result.message, fields: changed.map((field) => field.field) },
      });
    }
  }

  const text = (field: string, value: string) =>
    setOpen((current) => (current === null ? null : { ...current, texts: { ...current.texts, [field]: value } }));

  /** The leading cell of a row: its edit button, or its save and drop and the marks of its last save. */
  const control = (row: TRow, rowId: string) => (
    <Show
      when={open()?.rowId === rowId ? open() : undefined}
      fallback={
        <Button
          type="button"
          variant="outline"
          size="sm"
          aria-label={`edit ${rowId}`}
          disabled={open() !== null}
          onClick={() => start(row, rowId)}
        >
          edit
        </Button>
      }
    >
      {(current) => (
        <div data-slot="table-edit" class="flex items-center gap-1">
          <Button type="button" size="sm" disabled={current().saving} onClick={() => void save()}>
            save
          </Button>
          <Button type="button" variant="ghost" size="sm" disabled={current().saving} onClick={() => setOpen(null)}>
            drop
          </Button>
          <Show when={current().refusal}>
            {(refusal) => (
              <span data-slot="table-row-refusal" class="truncate text-sm text-destructive">
                {refusal().message}
              </span>
            )}
          </Show>
          <Show when={current().conflict}>
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

  /** The editor of one field of the open row. */
  const editor = (declared: TableColumn<TRow>, rowId: string, current: () => OpenRow) => {
    const field = declared.field;
    const choices = editable(field)?.choices;
    const error = () => current().errors[field];
    const invalid = () => error() !== undefined || current().refusal?.fields.includes(field) === true;
    return (
      <div data-slot="table-cell-edit" data-invalid={invalid() ? "true" : undefined} class="flex items-center gap-1">
        {choices === undefined ? (
          <Input
            class="h-8 min-w-24"
            aria-label={`${declared.label} ${rowId}`}
            aria-invalid={invalid() ? "true" : undefined}
            value={current().texts[field] ?? ""}
            disabled={current().saving}
            onInput={(event) => text(field, event.currentTarget.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter") {
                void save();
              } else if (event.key === "Escape") {
                setOpen(null);
              }
            }}
          />
        ) : (
          <ChoiceEditor
            label={`${declared.label} ${rowId}`}
            choices={choices}
            transport={transport}
            value={current().texts[field] ?? ""}
            invalid={invalid()}
            onChange={(value) => text(field, value)}
            readRow={readRow(field, choices.keyField, choices.displayField)}
          />
        )}
        <Show when={error()}>
          {(shown) => (
            <span data-slot="table-cell-refusal" class="truncate text-sm text-destructive">
              {shown()}
            </span>
          )}
        </Show>
      </div>
    );
  };

  /** The cell of a declared column: what it shows, and an editor while its row is open. */
  const cell = (declared: TableColumn<TRow>, shown: (row: TRow) => JSX.Element) =>
    editable(declared.field) === undefined
      ? shown
      : (row: TRow, rowId: string) => (
          <Show when={open()?.rowId === rowId ? open() : undefined} fallback={shown(row)}>
            {(current) => editor(declared, rowId, current)}
          </Show>
        );

  return {
    open: () => open() !== null,
    cell,
    /** The leading column, when the definition has an update. */
    columns: (): BuiltColumn<TRow>[] =>
      update === undefined ? [] : [{ id: EDIT_COLUMN, size: 150, header: () => "", cell: control }],
  };
}
