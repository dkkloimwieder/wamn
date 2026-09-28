/**
 * The grid part of a table's view: the column order, the hidden columns, the
 * widths a drag set, and the columns pinned to the start or the end. The grid
 * applies it in every mode, so the column panel works whether or not the set
 * is fully read.
 */

import { type ViewPart, declaredFields, fieldList, fieldPairs, same } from "./view-parts";

export interface GridViewState {
  /** Every declared column, in the table's order. */
  readonly order: readonly string[];
  readonly hidden: readonly string[];
  /** The width of each column that a drag set, in pixels. */
  readonly widths: Readonly<Record<string, number>>;
  readonly left: readonly string[];
  readonly right: readonly string[];
}

/** The grid part of a table definition: its columns in order, and the ones it hides. */
export const defaultGridView = (fields: readonly string[], hidden: readonly string[] = []): GridViewState => ({
  order: [...fields],
  hidden: [...hidden],
  widths: {},
  left: [],
  right: [],
});

export const GRID_VIEW: ViewPart<GridViewState> = {
  parts: ["order", "hidden", "width", "left", "right"],

  declared: (state, declaration) => {
    const fields = declaredFields(declaration);
    const declared = (field: string) => fields.includes(field);
    const kept = state.order.filter(declared);
    return {
      order: [...new Set([...kept, ...fields.filter((field) => !kept.includes(field))])],
      hidden: state.hidden.filter(declared),
      widths: Object.fromEntries(Object.entries(state.widths).filter(([field, width]) => declared(field) && width > 0)),
      left: state.left.filter(declared),
      right: state.right.filter((field) => declared(field) && !state.left.includes(field)),
    };
  },

  encode: (state, defaults) => {
    const out: [string, string][] = [];
    if (!same(state.order, defaults.order)) out.push(["order", state.order.join(",")]);
    if (!same([...state.hidden].sort(), [...defaults.hidden].sort())) {
      out.push(["hidden", defaults.order.filter((field) => state.hidden.includes(field)).join(",")]);
    }
    if (!same(state.widths, defaults.widths)) {
      out.push([
        "width",
        defaults.order
          .filter((field) => field in state.widths)
          .map((field) => `${field}:${state.widths[field]}`)
          .join(","),
      ]);
    }
    if (!same(state.left, defaults.left)) out.push(["left", state.left.join(",")]);
    if (!same(state.right, defaults.right)) out.push(["right", state.right.join(",")]);
    return out;
  },

  reader: (defaults, declaration) => {
    const fields = declaredFields(declaration);
    const state: { -readonly [K in keyof GridViewState]: GridViewState[K] } = { ...defaults };
    return {
      read: ([part, ...rest], value) => {
        if (rest.length > 0) {
          return false;
        }
        if (part === "width") {
          const widths = fieldPairs(value, fields, (_, text) =>
            /^\d+$/.test(text) && Number(text) > 0 ? Number(text) : null,
          );
          if (widths !== null) state.widths = Object.fromEntries(widths);
          return widths !== null;
        }
        const items = fieldList(value, fields);
        if (items !== null && (part === "order" || part === "hidden" || part === "left" || part === "right")) {
          state[part] = items;
          return true;
        }
        return false;
      },
      done: () => state,
    };
  },
};
