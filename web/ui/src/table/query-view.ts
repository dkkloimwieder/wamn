/**
 * The query part of a table's view: what the read is asked for. The sort, the
 * scope filters, the server search and the row cap. A change of any of them,
 * on a set that is not fully read, starts a new load. On a fully read set, the
 * table sorts the rows it has.
 *
 * A list scope filter is one URL key for each value, `pallets.scope.<field>=<value>`.
 * A range is `pallets.scope.<field>.min=<value>` and `.max`, and an is-null
 * filter is `pallets.scope.<field>.empty=true` or `false`. The server search
 * is `pallets.find=<text>`.
 */

import type { TableSort } from "./columns";
import type { ScopeFilter, ScopeRange } from "./scope-bar";
import { type ViewPart, declaredFields, fieldPairs, same } from "./view-parts";

export interface QueryViewState {
  readonly sort: readonly TableSort[];
  readonly scope: readonly ScopeFilter[];
  /** The server search, or empty. */
  readonly find: string;
  readonly cap: number;
}

/** The query part of a table definition: no sort, no scope, and the cap. */
export const defaultQueryView = (cap: number): QueryViewState => ({ sort: [], scope: [], find: "", cap });

/** True when a scope filter holds a value, a bound or a choice. */
const scoped = (scope: ScopeFilter) =>
  scope.values.length > 0 ||
  scope.empty !== undefined ||
  (scope.range !== undefined && (scope.range.min !== "" || scope.range.max !== ""));

export const QUERY_VIEW: ViewPart<QueryViewState> = {
  parts: ["sort", "scope", "find", "cap"],

  declared: (state, declaration) => {
    const fields = declaredFields(declaration);
    return {
      sort: state.sort.filter((sort) => fields.includes(sort.field)),
      scope: state.scope.filter((scope) => declaration.scopeFilters.includes(scope.field) && scoped(scope)),
      find: state.find,
      cap: Number.isInteger(state.cap) && state.cap > 0 ? state.cap : 1,
    };
  },

  encode: (state, defaults) => {
    const out: [string, string][] = [];
    if (!same(state.sort, defaults.sort)) {
      out.push([
        "sort",
        state.sort.map((sort) => `${sort.field}:${sort.direction === "descending" ? "desc" : "asc"}`).join(","),
      ]);
    }
    if (!same(state.scope, defaults.scope)) {
      for (const scope of state.scope) {
        for (const value of scope.values) out.push([`scope.${scope.field}`, value]);
        if (scope.range?.min) out.push([`scope.${scope.field}.min`, scope.range.min]);
        if (scope.range?.max) out.push([`scope.${scope.field}.max`, scope.range.max]);
        if (scope.empty !== undefined) out.push([`scope.${scope.field}.empty`, String(scope.empty)]);
      }
    }
    if (state.find !== defaults.find) out.push(["find", state.find]);
    if (state.cap !== defaults.cap) out.push(["cap", String(state.cap)]);
    return out;
  },

  reader: (defaults, declaration) => {
    const fields = declaredFields(declaration);
    const state: { -readonly [K in keyof QueryViewState]: QueryViewState[K] } = { ...defaults };
    const values = new Map<string, string[]>();
    const ranges = new Map<string, ScopeRange>();
    const empties = new Map<string, boolean>();
    const scopeField = (field: string | undefined): field is string =>
      field !== undefined && declaration.scopeFilters.includes(field);
    return {
      read: ([part, field, ...rest], value) => {
        if (part === "scope") {
          // A range bound or an is-null choice of a scope filter names its member.
          if (rest.length === 1 && scopeField(field)) {
            const [member] = rest;
            if ((member === "min" || member === "max") && value !== "") {
              ranges.set(field, { ...(ranges.get(field) ?? { min: "", max: "" }), [member]: value });
              return true;
            }
            if (member === "empty" && (value === "true" || value === "false")) {
              empties.set(field, value === "true");
              return true;
            }
            return false;
          }
          if (rest.length > 0 || field === undefined || !fields.includes(field) || !scopeField(field) || value === "") {
            return false;
          }
          values.set(field, [...(values.get(field) ?? []), value]);
          return true;
        }
        if (field !== undefined) {
          return false;
        }
        switch (part) {
          case "sort": {
            const sorts = fieldPairs(value, fields, (_, text) =>
              text === "asc" ? "ascending" : text === "desc" ? "descending" : null,
            );
            if (sorts !== null) state.sort = sorts.map(([sortField, direction]) => ({ field: sortField, direction }));
            return sorts !== null;
          }
          case "find":
            state.find = value;
            return true;
          case "cap":
            if (/^\d+$/.test(value) && Number(value) > 0) state.cap = Number(value);
            return /^\d+$/.test(value) && Number(value) > 0;
          default:
            return false;
        }
      },
      done: () => {
        if (values.size > 0 || ranges.size > 0 || empties.size > 0) {
          state.scope = declaration.scopeFilters
            .filter((field) => values.has(field) || ranges.has(field) || empties.has(field))
            .map((field) => {
              const range = ranges.get(field);
              const empty = empties.get(field);
              return {
                field,
                values: values.get(field) ?? [],
                ...(range === undefined ? {} : { range }),
                ...(empty === undefined ? {} : { empty }),
              };
            });
        }
        return state;
      },
    };
  },
};
