/**
 * The views of a DataTable (wamn-9v2r.2): a named copy of the table state.
 *
 * A view holds the columns (order, hidden, width, pin), the sort, the scope
 * filters, the refine filters, the search, the group levels, the chosen
 * aggregates and the cap. It names only declared fields: a field that the
 * table does not declare is dropped when the view is applied.
 *
 * A top-level table also writes its state to the URL, under its URL key. The
 * encoding is canonical: the parts come in one fixed order, and only a part
 * that differs from the default is written. For example:
 *
 *   ?pallets.order=code,createdAt,id&pallets.sort=createdAt:desc&pallets.cap=5000
 *
 * A scope filter is one key for each value, `pallets.scope.<field>=<value>`.
 * A refine filter is `pallets.filter.<field>=<kind>:<value>`. Reading ignores
 * an unknown part, an undeclared field and a value it cannot read, and names
 * each one, so the table can report them.
 */

import type { DataTableAggregate, DataTableBucket } from "./aggregate";
import type { DataTableFilter } from "./column-filter";
import type { DataTableGroupSort } from "./group-bar";
import type { DataTableScopeFilter } from "./scope-bar";

export type DataTableViewDirection = "ascending" | "descending";

/** One group level: its field, its time bucket, and how its groups sort. */
export interface DataTableViewGroup {
  readonly field: string;
  /** The bucket of a time field, or null for any other field. */
  readonly bucket: DataTableBucket | null;
  readonly sort: DataTableGroupSort;
}

/** The whole state a view saves. */
export interface DataTableViewState {
  /** Every column, in the table's order. */
  readonly order: readonly string[];
  readonly hidden: readonly string[];
  /** The width of each column that a drag set, in pixels. */
  readonly widths: Readonly<Record<string, number>>;
  readonly left: readonly string[];
  readonly right: readonly string[];
  readonly sort: readonly { readonly field: string; readonly direction: DataTableViewDirection }[];
  readonly scope: readonly DataTableScopeFilter[];
  readonly filters: readonly { readonly field: string; readonly filter: DataTableFilter }[];
  readonly search: string;
  readonly group: readonly DataTableViewGroup[];
  /** The aggregate of each column whose choice differs from its default. */
  readonly aggregates: Readonly<Record<string, DataTableAggregate>>;
  readonly cap: number;
}

/** A named view. */
export interface DataTableView {
  readonly name: string;
  readonly state: DataTableViewState;
}

/** What the table declares. A view is checked against it. */
export interface DataTableViewDeclaration {
  readonly columns: readonly {
    readonly field: string;
    /** True for a time column, which groups by a bucket. */
    readonly time: boolean;
    readonly groupable: boolean;
    readonly aggregates: readonly DataTableAggregate[];
  }[];
  readonly scopeFilters: readonly string[];
}

const BUCKETS: readonly DataTableBucket[] = ["day", "week", "month"];

/** The parts of the URL, in the order the encoding writes them. */
const PARTS = [
  "order",
  "hidden",
  "width",
  "left",
  "right",
  "sort",
  "scope",
  "filter",
  "search",
  "group",
  "aggregate",
  "cap",
] as const;

const same = (a: unknown, b: unknown) => JSON.stringify(a) === JSON.stringify(b);

/** The state with every undeclared field dropped, and every column in the order. */
export function declaredView(
  state: DataTableViewState,
  declaration: DataTableViewDeclaration,
): DataTableViewState {
  const fields = declaration.columns.map((column) => column.field);
  const declared = (field: string) => fields.includes(field);
  const column = (field: string) => declaration.columns.find((candidate) => candidate.field === field);
  const kept = state.order.filter(declared);
  return {
    order: [...new Set([...kept, ...fields.filter((field) => !kept.includes(field))])],
    hidden: state.hidden.filter(declared),
    widths: Object.fromEntries(
      Object.entries(state.widths).filter(([field, width]) => declared(field) && width > 0),
    ),
    left: state.left.filter(declared),
    right: state.right.filter((field) => declared(field) && !state.left.includes(field)),
    sort: state.sort.filter((sort) => declared(sort.field)),
    scope: state.scope.filter(
      (scope) => declaration.scopeFilters.includes(scope.field) && scope.values.length > 0,
    ),
    filters: state.filters.filter((filter) => declared(filter.field)),
    search: state.search,
    group: state.group
      .filter((level) => column(level.field)?.groupable === true)
      .map((level) => ({
        field: level.field,
        bucket: column(level.field)!.time ? (level.bucket ?? "day") : null,
        sort:
          level.sort.by === "value" || declared(level.sort.by)
            ? level.sort
            : { by: "value", descending: level.sort.descending },
      })),
    aggregates: Object.fromEntries(
      Object.entries(state.aggregates).filter(([field, aggregate]) =>
        column(field)?.aggregates.includes(aggregate),
      ),
    ),
    cap: Number.isInteger(state.cap) && state.cap > 0 ? state.cap : 1,
  };
}

const direction = (value: DataTableViewDirection) => (value === "descending" ? "desc" : "asc");

function filterText(filter: DataTableFilter): string {
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

/**
 * The URL parameters of a state, under its key: only the parts that differ
 * from the default, in the fixed order.
 */
export function encodeView(
  key: string,
  state: DataTableViewState,
  defaults: DataTableViewState,
): [string, string][] {
  const out: [string, string][] = [];
  const put = (part: string, value: string) => out.push([`${key}.${part}`, value]);
  const inOrder = <T>(record: Readonly<Record<string, T>>) =>
    defaults.order.filter((field) => field in record).map((field) => [field, record[field]!] as const);
  for (const part of PARTS) {
    switch (part) {
      case "order":
        if (!same(state.order, defaults.order)) put(part, state.order.join(","));
        break;
      case "hidden":
        if (!same([...state.hidden].sort(), [...defaults.hidden].sort())) {
          put(part, defaults.order.filter((field) => state.hidden.includes(field)).join(","));
        }
        break;
      case "width":
        if (!same(state.widths, defaults.widths)) {
          put(part, inOrder(state.widths).map(([field, width]) => `${field}:${width}`).join(","));
        }
        break;
      case "left":
      case "right":
        if (!same(state[part], defaults[part])) put(part, state[part].join(","));
        break;
      case "sort":
        if (!same(state.sort, defaults.sort)) {
          put(part, state.sort.map((sort) => `${sort.field}:${direction(sort.direction)}`).join(","));
        }
        break;
      case "scope":
        if (!same(state.scope, defaults.scope)) {
          for (const scope of state.scope) {
            for (const value of scope.values) put(`scope.${scope.field}`, value);
          }
        }
        break;
      case "filter":
        if (!same(state.filters, defaults.filters)) {
          for (const filter of state.filters) put(`filter.${filter.field}`, filterText(filter.filter));
        }
        break;
      case "search":
        if (state.search !== defaults.search) put(part, state.search);
        break;
      case "group":
        if (!same(state.group, defaults.group)) {
          put(
            part,
            state.group
              .map(
                (level) =>
                  `${level.field}:${level.bucket ?? ""}:${level.sort.by}:${level.sort.descending ? "desc" : "asc"}`,
              )
              .join(","),
          );
        }
        break;
      case "aggregate":
        if (!same(state.aggregates, defaults.aggregates)) {
          put(part, inOrder(state.aggregates).map(([field, aggregate]) => `${field}:${aggregate}`).join(","));
        }
        break;
      case "cap":
        if (state.cap !== defaults.cap) put(part, String(state.cap));
        break;
    }
  }
  return out;
}

function readFilter(text: string): DataTableFilter | null {
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

/**
 * The state that the URL parameters under a key set, over the defaults, and
 * each parameter it ignored, as `key=value`.
 */
export function decodeView(
  key: string,
  params: URLSearchParams,
  defaults: DataTableViewState,
  declaration: DataTableViewDeclaration,
): { state: DataTableViewState; ignored: string[] } {
  const ignored: string[] = [];
  const fields = declaration.columns.map((column) => column.field);
  const column = (field: string) => declaration.columns.find((candidate) => candidate.field === field);
  const state: { -readonly [K in keyof DataTableViewState]: DataTableViewState[K] } = { ...defaults };
  const scope = new Map<string, string[]>();
  const filters: { field: string; filter: DataTableFilter }[] = [];
  /** The fields of a list, or null when one is not declared. */
  const list = (value: string) => {
    const items = value === "" ? [] : value.split(",");
    return items.every((field) => fields.includes(field)) ? items : null;
  };
  /** The `field:value` pairs of a list, or null when one does not read. */
  const pairs = <T>(value: string, read: (field: string, text: string) => T | null) => {
    const out: [string, T][] = [];
    for (const item of value.split(",")) {
      const colon = item.indexOf(":");
      const field = item.slice(0, colon);
      const parsed = colon > 0 && fields.includes(field) ? read(field, item.slice(colon + 1)) : null;
      if (parsed === null) return null;
      out.push([field, parsed]);
    }
    return out;
  };
  const entries: [string, string][] = [];
  params.forEach((value, name) => entries.push([name, value]));
  for (const [name, value] of entries) {
    if (!name.startsWith(`${key}.`)) continue;
    const [part, field, ...rest] = name.slice(key.length + 1).split(".");
    const ignore = () => ignored.push(`${name}=${value}`);
    if (rest.length > 0) {
      ignore();
      continue;
    }
    if (part === "scope" || part === "filter") {
      if (field === undefined || !fields.includes(field)) {
        ignore();
      } else if (part === "scope") {
        if (declaration.scopeFilters.includes(field) && value !== "") {
          scope.set(field, [...(scope.get(field) ?? []), value]);
        } else ignore();
      } else {
        const filter = readFilter(value);
        if (filter !== null && !filters.some((kept) => kept.field === field)) {
          filters.push({ field, filter });
        } else ignore();
      }
      continue;
    }
    if (field !== undefined) {
      ignore();
      continue;
    }
    let read = true;
    switch (part) {
      case "order":
      case "hidden":
      case "left":
      case "right": {
        const items = list(value);
        if (items === null) read = false;
        else state[part] = items;
        break;
      }
      case "width": {
        const widths = pairs(value, (_, text) => (/^\d+$/.test(text) && Number(text) > 0 ? Number(text) : null));
        if (widths === null) read = false;
        else state.widths = Object.fromEntries(widths);
        break;
      }
      case "sort": {
        const sorts = pairs(value, (_, text) =>
          text === "asc" ? "ascending" : text === "desc" ? "descending" : null,
        );
        if (sorts === null) read = false;
        else state.sort = sorts.map(([sortField, sortDirection]) => ({ field: sortField, direction: sortDirection }));
        break;
      }
      case "search":
        state.search = value;
        break;
      case "group": {
        const levels: DataTableViewGroup[] = [];
        for (const item of value === "" ? [] : value.split(",")) {
          const [groupField = "", bucket = "", by = "", dir = ""] = item.split(":");
          const groupColumn = column(groupField);
          const bucketOk = groupColumn?.time ? BUCKETS.includes(bucket as DataTableBucket) : bucket === "";
          if (
            groupColumn?.groupable !== true ||
            !bucketOk ||
            !(by === "value" || fields.includes(by)) ||
            !(dir === "asc" || dir === "desc")
          ) {
            read = false;
            break;
          }
          levels.push({
            field: groupField,
            bucket: groupColumn.time ? (bucket as DataTableBucket) : null,
            sort: { by, descending: dir === "desc" },
          });
        }
        if (read) state.group = levels;
        break;
      }
      case "aggregate": {
        const chosen = pairs(value, (aggregateField, text) =>
          column(aggregateField)!.aggregates.includes(text as DataTableAggregate)
            ? (text as DataTableAggregate)
            : null,
        );
        if (chosen === null) read = false;
        else state.aggregates = Object.fromEntries(chosen);
        break;
      }
      case "cap":
        if (/^\d+$/.test(value) && Number(value) > 0) state.cap = Number(value);
        else read = false;
        break;
      default:
        read = false;
    }
    if (!read) ignore();
  }
  if (scope.size > 0) {
    state.scope = declaration.scopeFilters
      .filter((field) => scope.has(field))
      .map((field) => ({ field, values: scope.get(field)! }));
  }
  if (filters.length > 0) state.filters = filters;
  return { state: declaredView(state, declaration), ignored };
}
