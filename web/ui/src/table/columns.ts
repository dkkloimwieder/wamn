/**
 * The columns of a platform table, and the one order of their values.
 *
 * A table definition declares its columns: a field, a label, a type and a
 * role. `QueryTable` builds each column as the grid renders it, a cell for
 * each row, and adds its control columns: the row selection, the expand of the
 * child tables, and the row buttons. `SetTable` takes those columns and adds
 * its own behavior over them without knowing what a cell does.
 */

import type { JSX } from "solid-js";

/** The type of a column, as the frozen `wamn:postgres/types.sql-value` names it. */
export type TableColumnType =
  "boolean" | "int32" | "int64" | "float64" | "text" | "bytes" | "numeric" | "timestamptz" | "json" | "uuid";

/**
 * What a column is to its row: the row id, a reference to another record, the
 * row's revision, or any other value.
 */
export type TableColumnRole = "key" | "reference" | "revision" | "value";

/** A sort direction, as a table definition names it. */
export type TableSortDirection = "ascending" | "descending";

/** One field of a sort, in the order of the sort. */
export interface TableSort<Field extends string = string> {
  readonly field: Field;
  readonly direction: TableSortDirection;
}

/** One declared column. */
export interface TableColumn<TRow extends object> {
  readonly field: keyof TRow & string;
  readonly label: string;
  readonly type: TableColumnType;
  /** The column's role. A column that states none is a value. */
  readonly role?: TableColumnRole | undefined;
  /**
   * What a cell shows in place of its value, for example the text of the
   * record a key names. The sort, the filters, the search and the export
   * still read the value.
   */
  readonly cell?: ((value: unknown) => JSX.Element) | undefined;
}

/** One column as the grid renders it: a declared column, or a control column. */
export interface BuiltColumn<TRow extends object> {
  readonly id: string;
  /** The declared column. A control column has none. */
  readonly declared?: TableColumn<TRow> | undefined;
  /** The starting width, before a drag sets one. */
  readonly size: number;
  /** The header of a control column. A declared column's header is its label. */
  readonly header?: (() => JSX.Element) | undefined;
  /** What one data row's cell shows. */
  readonly cell: (row: TRow, rowId: string) => JSX.Element;
}

/** The ids of the control columns before the first declared column, which lead the order. */
export function leadingIds(columns: readonly BuiltColumn<object>[]): string[] {
  const first = columns.findIndex((built) => built.declared !== undefined);
  return columns.slice(0, first < 0 ? columns.length : first).map((built) => built.id);
}

/** The shown text of one value. */
export const shownText = (value: unknown): string => (value === null || value === undefined ? "" : String(value));

/** The starting width of a declared column: a whole id or time fits. */
export const startWidth = (type: TableColumnType): number =>
  type === "uuid" ? 300 : type === "timestamptz" ? 240 : 150;

/** The declared columns as the grid renders them, each cell showing its value. */
export function builtColumns<TRow extends object>(columns: readonly TableColumn<TRow>[]): BuiltColumn<TRow>[] {
  return columns.map((declared) => ({
    id: declared.field,
    declared,
    size: startWidth(declared.type),
    cell: (row) => declared.cell?.(row[declared.field]) ?? shownText(row[declared.field]),
  }));
}

export const isEmpty = (value: unknown): boolean => value === null || value === undefined || value === "";

/** A decimal as whole units of its scale: 12.5 at scale 2 is 1250. */
export interface Decimal {
  readonly units: bigint;
  readonly scale: number;
}

/** The number of decimal places of one decimal text. */
export function decimalScale(text: string): number {
  const point = text.indexOf(".");
  return point === -1 ? 0 : text.length - point - 1;
}

export function parseDecimal(text: string): Decimal {
  const trimmed = text.trim();
  const point = trimmed.indexOf(".");
  const digits = point === -1 ? trimmed : trimmed.slice(0, point) + trimmed.slice(point + 1);
  return { units: BigInt(digits), scale: decimalScale(trimmed) };
}

/** The units of a decimal at a scale at least its own. */
export const unitsAt = (decimal: Decimal, scale: number): bigint =>
  decimal.units * 10n ** BigInt(scale - decimal.scale);

const order = <T>(a: T, b: T) => (a < b ? -1 : a > b ? 1 : 0);

/**
 * The order of two values of one column type. A decimal compares exactly, an
 * int64 as a whole number, and a time as its instant. A null sorts after
 * every value, so a descending sort puts it first, as Postgres does.
 */
export function compareValues(type: TableColumnType, a: unknown, b: unknown): number {
  const isNull = (value: unknown) => value === null || value === undefined;
  if (isNull(a) || isNull(b)) {
    return (isNull(a) ? 1 : 0) - (isNull(b) ? 1 : 0);
  }
  switch (type) {
    case "numeric": {
      const [x, y] = [parseDecimal(String(a)), parseDecimal(String(b))];
      const scale = Math.max(x.scale, y.scale);
      return order(unitsAt(x, scale), unitsAt(y, scale));
    }
    case "int64":
      return order(BigInt(String(a)), BigInt(String(b)));
    case "timestamptz":
      return order(Date.parse(String(a)), Date.parse(String(b)));
    case "int32":
    case "float64":
    case "boolean":
      return order(Number(a), Number(b));
    default:
      return order(String(a), String(b));
  }
}
