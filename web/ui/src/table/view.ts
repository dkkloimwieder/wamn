/**
 * The views of a table: a named copy of its state, and its state in the URL
 * (wamn-9v2r.2).
 *
 * The state has three parts, and each part has one owner, which defaults it,
 * reads it and writes it:
 *
 * - the grid: the column order, the hidden columns, the widths and the pins,
 *   in `grid-view.ts`;
 * - the query: the sort, the scope filters, the server search and the cap, in
 *   `query-view.ts`;
 * - the set: the refine filters, the search, the group levels and the chosen
 *   aggregates, in `set-view.ts`.
 *
 * A view names only declared fields: a field that the table does not declare
 * is dropped when the view is applied.
 *
 * A top-level table also writes its state to the URL, under its URL key. The
 * encoding is canonical: the parts come in one fixed order, and only a part
 * that differs from the default is written. For example:
 *
 *   ?pallets.order=code,createdAt,id&pallets.sort=createdAt:desc&pallets.cap=5000
 *
 * Reading ignores an unknown part, an undeclared field and a value it cannot
 * read, and names each one, in the order of the URL, so the table can report
 * them.
 */

import { GRID_VIEW, type GridViewState } from "./grid-view";
import { QUERY_VIEW, type QueryViewState } from "./query-view";
import { SET_VIEW, type SetViewState } from "./set-view";
import type { ViewDeclaration } from "./view-parts";

export type { ViewDeclaration } from "./view-parts";

/** The whole state a view saves. */
export interface TableView {
  readonly grid: GridViewState;
  readonly query: QueryViewState;
  readonly set: SetViewState;
}

/** A named view. */
export interface NamedView {
  readonly name: string;
  readonly view: TableView;
}

/** The parts of the URL, in the order the encoding writes them. */
const PARTS = [...GRID_VIEW.parts, "sort", "scope", "find", ...SET_VIEW.parts, "cap"] as const;

/** The state with every undeclared field dropped. */
export function declaredView(view: TableView, declaration: ViewDeclaration): TableView {
  return {
    grid: GRID_VIEW.declared(view.grid, declaration),
    query: QUERY_VIEW.declared(view.query, declaration),
    set: SET_VIEW.declared(view.set, declaration),
  };
}

/**
 * The URL parameters of a state, under its key: only the parts that differ
 * from the default, in the fixed order.
 */
export function encodeView(
  key: string,
  view: TableView,
  defaults: TableView,
  declaration: ViewDeclaration,
): [string, string][] {
  const written = [
    ...GRID_VIEW.encode(view.grid, defaults.grid, declaration),
    ...QUERY_VIEW.encode(view.query, defaults.query, declaration),
    ...SET_VIEW.encode(view.set, defaults.set, declaration),
  ];
  const rank = (name: string) => PARTS.indexOf(name.split(".")[0] as (typeof PARTS)[number]);
  // A stable sort keeps each owner's own order inside one part.
  return written
    .map(([name, value], index) => ({ name, value, index }))
    .sort((a, b) => rank(a.name) - rank(b.name) || a.index - b.index)
    .map(({ name, value }) => [`${key}.${name}`, value]);
}

/**
 * The state that the URL parameters under a key set, over the defaults, and
 * each parameter it ignored, as `key=value`.
 */
export function decodeView(
  key: string,
  params: URLSearchParams,
  defaults: TableView,
  declaration: ViewDeclaration,
): { view: TableView; ignored: string[] } {
  const ignored: string[] = [];
  const readers = {
    grid: GRID_VIEW.reader(defaults.grid, declaration),
    query: QUERY_VIEW.reader(defaults.query, declaration),
    set: SET_VIEW.reader(defaults.set, declaration),
  };
  const owner = (part: string | undefined) =>
    GRID_VIEW.parts.includes(part ?? "")
      ? readers.grid
      : SET_VIEW.parts.includes(part ?? "")
        ? readers.set
        : QUERY_VIEW.parts.includes(part ?? "")
          ? readers.query
          : undefined;
  params.forEach((value, name) => {
    if (!name.startsWith(`${key}.`)) {
      return;
    }
    const split = name.slice(key.length + 1).split(".");
    if (owner(split[0])?.read(split, value) !== true) {
      ignored.push(`${name}=${value}`);
    }
  });
  const view = { grid: readers.grid.done(), query: readers.query.done(), set: readers.set.done() };
  return { view: declaredView(view, declaration), ignored };
}
