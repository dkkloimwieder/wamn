/**
 * The DataTable of one generated read, wired from its table definition alone
 * (wamn-sa7d.1).
 *
 * The emitter writes the definition as data: the read and every operation the
 * table calls, as bindings, the input paths of the limit, the sort and each
 * scope filter, the record read of each column that names a record, the
 * update a cell edits through, the operations a row opens, and the child
 * tables. This component does the rest, so no screen carries table code.
 *
 * The table loads when it mounts, and again after every write on the
 * transport that it did not send itself. A filter that the caller fixes is
 * not offered in the scope bar. A child table is this component again, with
 * its scope filter fixed to the parent row's key.
 */

import { type JSX, Show, createSignal, onCleanup } from "solid-js";

import {
  afterWrites,
  boundedPage,
  callOperation,
  type JsonValue,
  type LoadPage,
  type MemberPath,
  type OperationBinding,
  type Outcome,
  readMember,
  refusalSentence,
  type RowKey,
  type SuppliedInput,
  type Transport,
  writeMember,
  writeSupplied,
} from "@wamn/web-runtime";

import { Button } from "../components/ui/button";
import { TableScreen } from "../actions";
import { announceOutcome } from "../outcome";
import { createRecordLabels } from "../record-labels";
import type { DataTableChild } from "./child-tables";
import { DataTable, type DataTableColumn } from "./data-table";
import type { DataTableEditResult } from "./edit-cell";
import type { DataTableScopeFilter } from "./scope-bar";
import { createTableLoad } from "./table-load";

/** One column of a definition. A column that names a record names the read of its text. */
export type QueryTableColumn<TRow extends object> = Omit<DataTableColumn<TRow>, "cell"> & {
  /** The member of the record read's result that the cell shows. */
  readonly displayField?: string;
  /** The read that returns one record by the key this column holds. */
  readonly recordRead?: { readonly read: OperationBinding; readonly keyInput: MemberPath };
};

/** One declared scope filter: the row member, its input path, and whether it takes a list. */
export interface QueryTableFilter {
  readonly field: string;
  readonly input: MemberPath;
  readonly list: boolean;
}

/** A row member and the input it fills. */
export interface QueryTableFill {
  readonly field: string;
  readonly input: MemberPath;
}

/** One operation a row opens: a record by the row, or a form the row fills. */
export interface QueryTableAction {
  readonly operation: string;
  readonly label: string;
  /** True when one call takes many rows. */
  readonly many: boolean;
  readonly opens: "record" | "form";
  /** The inputs of the form that the row fills. */
  readonly fill: readonly QueryTableFill[];
}

/** The update a cell edits through. */
export interface QueryTableUpdate {
  readonly binding: OperationBinding;
  /** The input that names the row, which takes the row's one key field. */
  readonly keyInput: MemberPath;
  readonly revisionInput?: MemberPath;
  readonly revisionField?: string;
  readonly supplied: readonly SuppliedInput[];
  /** Each editable column and the input it writes. */
  readonly fields: readonly QueryTableFill[];
}

/** One child table: its definition, and the column of its scope filter that names the parent. */
export interface QueryTableChild {
  readonly label: string;
  /** Returns the child's definition. A function, so modules that import each other load. */
  readonly table: () => QueryTableDefinition<any>;
  readonly scopeFilter: string;
}

/** The table definition the emitter writes for one read. */
export interface QueryTableDefinition<TRow extends object> {
  /** The table's name, which the file name of a CSV export starts with. */
  readonly name: string;
  readonly read: OperationBinding;
  /** `item` for one page of a paged read, `rows` for every row of a bounded read. */
  readonly rows: "item" | "rows";
  readonly rowId: RowKey<TRow>;
  readonly pageMaximum: number | null;
  readonly limitInput: MemberPath | null;
  readonly sortFieldInput: MemberPath | null;
  readonly sortDirectionInput: MemberPath | null;
  readonly filters: readonly QueryTableFilter[];
  readonly scopeFilters: readonly (keyof TRow & string)[];
  readonly sortFields: readonly { readonly field: keyof TRow & string; readonly wire: string }[];
  readonly sortMaxFields: number;
  readonly columns: readonly QueryTableColumn<TRow>[];
  readonly update?: QueryTableUpdate;
  readonly actions: readonly QueryTableAction[];
  readonly childTables: readonly QueryTableChild[];
}

export interface QueryTableProps<TRow extends object, TResult = unknown> {
  readonly definition: QueryTableDefinition<TRow>;
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** What an operator calls this screen, which its outcomes name. */
  readonly label: string;
  /** Input the parent fixes, which the operator does not edit. */
  readonly fixed?: object | undefined;
  /** Called when the operator opens an operation's record from one row. */
  readonly onOpen?: ((operation: string, row: TRow) => void) | undefined;
  /** Called with the values one row hands to an operation's form. */
  readonly onFill?: ((operation: string, initial: object) => void) | undefined;
  /** Called with every outcome of the read. */
  readonly onOutcome?: ((outcome: Outcome<TResult>) => void) | undefined;
}

type Member = { readonly [name: string]: unknown };

/** What one outcome of an inline edit means to its cell. */
function editResult<TRow>(outcome: Outcome<unknown>, row: TRow): DataTableEditResult<TRow> {
  switch (outcome.status) {
    case "completed": {
      // The row takes the members the write returned, and keeps the rest.
      const written = (outcome.value ?? {}) as Member;
      const next = { ...row } as Member & TRow;
      for (const name of Object.keys(next)) {
        if (name in written) {
          (next as { [name: string]: unknown })[name] = written[name];
        }
      }
      return { status: "completed", row: next };
    }
    case "refused":
      return {
        status: outcome.code === "concurrency_conflict" ? "conflict" : "refused",
        message: refusalSentence(outcome.code),
      };
    case "uncertain":
      return { status: "uncertain", message: outcome.reason };
    default:
      return { status: "uncertain", message: "partially completed" };
  }
}

export function QueryTable<TRow extends object, TResult = unknown>(
  props: QueryTableProps<TRow, TResult>,
): JSX.Element {
  // The definition and the transport name one table for its whole life.
  const definition = props.definition;
  const transport = props.transport;
  const [scope, setScope] = createSignal<readonly DataTableScopeFilter[]>([]);
  // True while this table sends a write, whose result it puts in place itself.
  let writing = false;

  const fixedBy = (filter: QueryTableFilter) => readMember(props.fixed ?? {}, filter.input) !== undefined;

  const load = createTableLoad<TRow>(definition, async (limit, sort) => {
    let request = { ...props.fixed } as object;
    for (const chosen of scope()) {
      const filter = definition.filters.find((declared) => declared.field === chosen.field);
      if (filter !== undefined && !fixedBy(filter)) {
        request = writeMember(request, filter.input, filter.list ? [...chosen.values] : chosen.values[0]!);
      }
    }
    if (definition.limitInput !== null) {
      request = writeMember(request, definition.limitInput, limit);
    }
    if (sort !== undefined) {
      if (definition.sortFieldInput !== null) {
        request = writeMember(request, definition.sortFieldInput, sort.field);
      }
      if (definition.sortDirectionInput !== null) {
        request = writeMember(request, definition.sortDirectionInput, sort.direction);
      }
    }
    const outcome = await callOperation<LoadPage<TRow> & { readonly rows: readonly TRow[] }>(
      transport,
      definition.read,
      [request],
    );
    props.onOutcome?.(outcome as Outcome<unknown> as Outcome<TResult>);
    if (outcome.status !== "completed") {
      announceOutcome(outcome, props.label);
    }
    return definition.rows === "rows" ? boundedPage(outcome) : outcome;
  });
  void load.load();
  onCleanup(
    afterWrites(transport, () => {
      if (!writing) {
        void load.load();
      }
    }),
  );

  // A column that names a record shows the text its record read returns.
  const columns: readonly DataTableColumn<TRow>[] = definition.columns.map((column) => {
    const { recordRead, displayField, ...shown } = column;
    if (recordRead === undefined || displayField === undefined) {
      return shown;
    }
    const labels = createRecordLabels(async (key) => {
      const outcome = await callOperation<Member>(transport, recordRead.read, [
        writeMember({}, recordRead.keyInput, key),
      ]);
      if (outcome.status !== "completed") {
        return null;
      }
      const text = outcome.value[displayField];
      return text == null ? null : String(text);
    });
    return { ...shown, cell: (value: unknown) => <>{labels(value as string | null)}</> };
  });

  const fill = (action: QueryTableAction, row: TRow) =>
    action.fill.reduce<object>(
      (initial, { field, input }) => writeMember(initial, input, (row as Member)[field] as JsonValue),
      {},
    );

  // The buttons of one row: each operation it opens, when the page takes it.
  const rowActions =
    definition.actions.length === 0
      ? undefined
      : (row: TRow) => (
          <>
            {definition.actions.map((action) => (
              <Show when={action.opens === "record" ? props.onOpen : props.onFill}>
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  onClick={() =>
                    action.opens === "record"
                      ? props.onOpen?.(action.operation, row)
                      : props.onFill?.(action.operation, fill(action, row))
                  }
                >
                  {action.label}
                </Button>
              </Show>
            ))}
          </>
        );

  const update = definition.update;
  const edit =
    update === undefined
      ? undefined
      : async (row: TRow, field: string, value: unknown): Promise<DataTableEditResult<TRow>> => {
          const target = update.fields.find((editable) => editable.field === field)!;
          const member = row as Member;
          let request = writeMember({}, update.keyInput, member[definition.rowId[0]!] as JsonValue);
          if (update.revisionInput !== undefined && update.revisionField !== undefined) {
            request = writeMember(request, update.revisionInput, member[update.revisionField] as JsonValue);
          }
          request = writeSupplied(writeMember(request, target.input, value as JsonValue), update.supplied);
          writing = true;
          try {
            return editResult(await callOperation(transport, update.binding, [request]), row);
          } finally {
            writing = false;
          }
        };

  // Each child is this table again, with its scope filter fixed to the row's key.
  const childTables: readonly DataTableChild[] = definition.childTables.map((child) => ({
    label: child.label,
    render: (key: string) => {
      const table = child.table();
      const filter = table.filters.find((declared) => declared.field === child.scopeFilter)!;
      return (
        <QueryTable
          definition={table}
          transport={transport}
          label={child.label}
          fixed={writeMember({}, filter.input, filter.list ? [key] : key)}
          onOpen={props.onOpen}
          onFill={props.onFill}
        />
      );
    },
  }));

  return (
    <TableScreen>
      <DataTable
        name={definition.name}
        columns={columns}
        rowId={definition.rowId}
        rows={load.state().rows}
        fullyRead={load.state().fullyRead}
        busy={load.state().busy}
        refusal={load.state().refusal}
        cap={load.state().cap}
        onCapChange={(cap) => void load.load(cap)}
        onRefresh={() => void load.load()}
        startedAt={load.state().startedAt}
        endedAt={load.state().endedAt}
        sortFields={definition.sortFields}
        sortMaxFields={definition.sortMaxFields}
        onSortChange={load.sortBy}
        scopeFilters={definition.scopeFilters.filter((field) => {
          const filter = definition.filters.find((declared) => declared.field === field);
          return filter === undefined || !fixedBy(filter);
        })}
        onScopeChange={(filters) => {
          setScope(filters);
          void load.load();
        }}
        rowActions={rowActions}
        editableFields={update?.fields.map((editable) => editable.field as keyof TRow & string)}
        onEdit={edit}
        onEditing={load.hold}
        onRowChange={load.replaceRow}
        onReload={() => void load.load()}
        childTables={childTables.length === 0 ? undefined : childTables}
      />
    </TableScreen>
  );
}
