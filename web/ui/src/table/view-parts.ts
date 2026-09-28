/**
 * What the three owners of a table's view state share: the declaration a view
 * is checked against, the shape of one owner, and the readers of a URL list.
 */

import type { Aggregate } from "./aggregate";

/** What the table declares. A view is checked against it. */
export interface ViewDeclaration {
  readonly columns: readonly {
    readonly field: string;
    /** True for a time column, which groups by a bucket. */
    readonly time: boolean;
    readonly groupable: boolean;
    readonly aggregates: readonly Aggregate[];
  }[];
  readonly scopeFilters: readonly string[];
}

/** The fields the declaration names, in its order. */
export const declaredFields = (declaration: ViewDeclaration) => declaration.columns.map((column) => column.field);

/** One owner of a part of the state. */
export interface ViewPart<State> {
  /** The URL parts it writes, in the order it writes them. */
  readonly parts: readonly string[];
  /** The state with every undeclared field dropped. */
  readonly declared: (state: State, declaration: ViewDeclaration) => State;
  /** The URL parameters of the state, without the key: only what differs from the default. */
  readonly encode: (state: State, defaults: State, declaration: ViewDeclaration) => [string, string][];
  /**
   * A reader over the defaults. `read` takes one parameter, split at its dots,
   * and returns false when it cannot read it. `done` returns the state read.
   */
  readonly reader: (
    defaults: State,
    declaration: ViewDeclaration,
  ) => { read: (name: readonly string[], value: string) => boolean; done: () => State };
}

export const same = (a: unknown, b: unknown) => JSON.stringify(a) === JSON.stringify(b);

/** The fields of a comma list, or null when one is not declared. */
export function fieldList(value: string, fields: readonly string[]): string[] | null {
  const items = value === "" ? [] : value.split(",");
  return items.every((field) => fields.includes(field)) ? items : null;
}

/** The `field:value` pairs of a comma list, or null when one does not read. */
export function fieldPairs<T>(
  value: string,
  fields: readonly string[],
  read: (field: string, text: string) => T | null,
): [string, T][] | null {
  const out: [string, T][] = [];
  for (const item of value.split(",")) {
    const colon = item.indexOf(":");
    const field = item.slice(0, colon);
    const parsed = colon > 0 && fields.includes(field) ? read(field, item.slice(colon + 1)) : null;
    if (parsed === null) return null;
    out.push([field, parsed]);
  }
  return out;
}
