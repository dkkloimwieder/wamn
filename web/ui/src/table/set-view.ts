/**
 * The set part of a table's view: what the table does with a fully read set.
 * The refine filters, the search, the group levels and the chosen aggregates.
 * On a set that is not fully read, the table keeps them and applies none.
 *
 * A refine filter is `pallets.filter.<field>=<kind>:<value>`.
 */

import { type Aggregate, type Bucket, BUCKETS } from "./aggregate";
import type { SetFilter } from "./column-filter";
import { type GroupSort, VALUE_SORT } from "./group-bar";
import { type ViewPart, declaredFields, fieldPairs, same } from "./view-parts";

/** One group level: its field, its time bucket, and how its groups sort. */
export interface GroupLevel {
  readonly field: string;
  /** The bucket of a time field, or null for any other field. */
  readonly bucket: Bucket | null;
  readonly sort: GroupSort;
}

export interface SetViewState {
  readonly filters: readonly { readonly field: string; readonly filter: SetFilter }[];
  readonly search: string;
  readonly group: readonly GroupLevel[];
  /** The aggregate of each column whose choice differs from its default. */
  readonly aggregates: Readonly<Record<string, Aggregate>>;
}

/** The set part of a table definition: the fields it groups, in nesting order. */
export const defaultSetView = (
  group: readonly { readonly field: string; readonly time: boolean }[] = [],
): SetViewState => ({
  filters: [],
  search: "",
  group: group.map((level) => ({ field: level.field, bucket: level.time ? "day" : null, sort: VALUE_SORT })),
  aggregates: {},
});

function filterText(filter: SetFilter): string {
  switch (filter.kind) {
    case "contains":
      return `contains:${filter.text}`;
    case "equals":
      return `equals:${filter.value}`;
    case "range":
      return `range:${filter.min},${filter.max}`;
    case "is":
      return `is:${filter.value}`;
    default:
      return filter.kind;
  }
}

function readFilter(text: string): SetFilter | null {
  const colon = text.indexOf(":");
  const kind = colon < 0 ? text : text.slice(0, colon);
  const value = colon < 0 ? null : text.slice(colon + 1);
  switch (kind) {
    case "contains":
      return value === null ? null : { kind, text: value };
    case "equals":
      return value === null ? null : { kind, value };
    case "range": {
      const bounds = value?.split(",");
      return bounds?.length === 2 ? { kind, min: bounds[0]!, max: bounds[1]! } : null;
    }
    case "is":
      return value === "true" || value === "false" ? { kind, value: value === "true" } : null;
    case "empty":
    case "not-empty":
      return value === null ? { kind } : null;
    default:
      return null;
  }
}

export const SET_VIEW: ViewPart<SetViewState> = {
  parts: ["filter", "search", "group", "aggregate"],

  declared: (state, declaration) => {
    const fields = declaredFields(declaration);
    const column = (field: string) => declaration.columns.find((candidate) => candidate.field === field);
    return {
      filters: state.filters.filter((filter) => fields.includes(filter.field)),
      search: state.search,
      group: state.group
        .filter((level) => column(level.field)?.groupable === true)
        .map((level) => ({
          field: level.field,
          bucket: column(level.field)!.time ? (level.bucket ?? "day") : null,
          sort:
            level.sort.by === "value" || fields.includes(level.sort.by)
              ? level.sort
              : { by: "value", descending: level.sort.descending },
        })),
      aggregates: Object.fromEntries(
        Object.entries(state.aggregates).filter(([field, aggregate]) => column(field)?.aggregates.includes(aggregate)),
      ),
    };
  },

  encode: (state, defaults, declaration) => {
    const out: [string, string][] = [];
    if (!same(state.filters, defaults.filters)) {
      for (const filter of state.filters) out.push([`filter.${filter.field}`, filterText(filter.filter)]);
    }
    if (state.search !== defaults.search) out.push(["search", state.search]);
    if (!same(state.group, defaults.group)) {
      out.push([
        "group",
        state.group
          .map(
            (level) =>
              `${level.field}:${level.bucket ?? ""}:${level.sort.by}:${level.sort.descending ? "desc" : "asc"}`,
          )
          .join(","),
      ]);
    }
    if (!same(state.aggregates, defaults.aggregates)) {
      out.push([
        "aggregate",
        declaredFields(declaration)
          .filter((field) => field in state.aggregates)
          .map((field) => `${field}:${state.aggregates[field]}`)
          .join(","),
      ]);
    }
    return out;
  },

  reader: (defaults, declaration) => {
    const fields = declaredFields(declaration);
    const column = (field: string) => declaration.columns.find((candidate) => candidate.field === field);
    const state: { -readonly [K in keyof SetViewState]: SetViewState[K] } = { ...defaults };
    const filters: { field: string; filter: SetFilter }[] = [];
    return {
      read: ([part, field, ...rest], value) => {
        if (rest.length > 0) {
          return false;
        }
        if (part === "filter") {
          const filter = field !== undefined && fields.includes(field) ? readFilter(value) : null;
          if (filter === null || filters.some((kept) => kept.field === field)) {
            return false;
          }
          filters.push({ field: field!, filter });
          return true;
        }
        if (field !== undefined) {
          return false;
        }
        switch (part) {
          case "search":
            state.search = value;
            return true;
          case "group": {
            const levels: GroupLevel[] = [];
            for (const item of value === "" ? [] : value.split(",")) {
              const [groupField = "", bucket = "", by = "", dir = ""] = item.split(":");
              const grouped = column(groupField);
              const bucketOk = grouped?.time ? BUCKETS.includes(bucket as Bucket) : bucket === "";
              if (
                grouped?.groupable !== true ||
                !bucketOk ||
                !(by === "value" || fields.includes(by)) ||
                !(dir === "asc" || dir === "desc")
              ) {
                return false;
              }
              levels.push({
                field: groupField,
                bucket: grouped.time ? (bucket as Bucket) : null,
                sort: { by, descending: dir === "desc" },
              });
            }
            state.group = levels;
            return true;
          }
          case "aggregate": {
            const chosen = fieldPairs(value, fields, (aggregateField, text) =>
              column(aggregateField)!.aggregates.includes(text as Aggregate) ? (text as Aggregate) : null,
            );
            if (chosen !== null) state.aggregates = Object.fromEntries(chosen);
            return chosen !== null;
          }
          default:
            return false;
        }
      },
      done: () => (filters.length > 0 ? { ...state, filters } : state),
    };
  },
};
