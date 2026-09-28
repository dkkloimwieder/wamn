/**
 * The table definition the emitter writes for one read (wamn-sa7d.1).
 *
 * The emitter writes it as data: the read and every operation the table calls,
 * as bindings, the input paths of the limit, the sort and each scope filter,
 * the record read of each column that names a record, the update a cell edits
 * through, the operations a row opens, and the child tables.
 */

import type { Component } from "solid-js";

import type { MemberPath, OperationBinding, RowKey, SuppliedInput } from "@wamn/web-runtime";

import type { TableColumn } from "./columns";

/** One column of a definition. A column that names a record names the read of its text. */
export type QueryTableColumn<TRow extends object> = Omit<TableColumn<TRow>, "cell"> & {
  /** The member of the record read's result that the cell shows. */
  readonly displayField?: string;
  /** The read that returns one record by the key this column holds. */
  readonly recordRead?: { readonly read: OperationBinding; readonly keyInput: MemberPath };
};

/** How a scope filter matches a value: exactly when the definition states none. */
export type QueryTableMatch = "contains" | "prefix" | "range" | "is_null";

/**
 * One declared scope filter: the row member, its input path, whether it takes
 * a list, and how it matches. A range states the contract type of its bounds.
 * A band is a required range, which reads the last `defaultLastDays` days when
 * a request leaves it out.
 */
export interface QueryTableFilter {
  readonly field: string;
  readonly input: MemberPath;
  readonly list: boolean;
  readonly match?: QueryTableMatch;
  readonly type?: string;
  readonly required?: boolean;
  readonly defaultLastDays?: number;
}

/** A read's server search: its input, and the row members it reads. */
export interface QueryTableSearch {
  readonly input: MemberPath;
  readonly fields: readonly string[];
}

/** A row member and the input it fills. A `"[]"` in the input marks a repeated member. */
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
  /** The revision the row carries for the record it fills, and the input that sends it. */
  readonly revision?: QueryTableFill;
  /**
   * Loads the form that sends one input for each of many rows, which a bulk
   * action opens. The form loads when the action first opens.
   */
  // eslint-disable-next-line @typescript-eslint/no-explicit-any -- a form or a table of any row shape.
  readonly form?: () => Promise<{ readonly default: Component<any> }>;
}

/**
 * The list that offers the records a reference field can name: its read, the
 * result member of its rows, the row member it stores and the one it shows,
 * and the inputs of its search and its page cursor.
 */
export interface QueryTableChoices {
  readonly read: OperationBinding;
  readonly rows: string;
  readonly keyField: string;
  readonly displayField: string;
  readonly searchInput?: MemberPath;
  readonly cursorInput?: MemberPath;
}

/** An editable column and the input it writes, and the list its records come from when it names a record. */
export interface QueryTableEditField extends QueryTableFill {
  readonly choices?: QueryTableChoices;
}

/** The update a row edits through. */
export interface QueryTableUpdate {
  readonly binding: OperationBinding;
  /** The input that names the row, which takes the row's one key field. */
  readonly keyInput: MemberPath;
  readonly revisionInput?: MemberPath;
  readonly revisionField?: string;
  readonly supplied: readonly SuppliedInput[];
  /** Each editable column and the input it writes. */
  readonly fields: readonly QueryTableEditField[];
}

/** One child table: its definition, and the column of its scope filter that names the parent. */
export interface QueryTableChild {
  readonly label: string;
  /** Returns the child's definition. A function, so modules that import each other load. */
  // eslint-disable-next-line @typescript-eslint/no-explicit-any -- a form or a table of any row shape.
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
  readonly search?: QueryTableSearch;
  readonly sortFields: readonly { readonly field: keyof TRow & string; readonly wire: string }[];
  readonly sortMaxFields: number;
  readonly columns: readonly QueryTableColumn<TRow>[];
  readonly update?: QueryTableUpdate;
  readonly actions: readonly QueryTableAction[];
  readonly childTables: readonly QueryTableChild[];
}
