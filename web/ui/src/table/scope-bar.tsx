/**
 * The scope bar of the DataTable (wamn-9v2r.1, wamn-3nsf.5).
 *
 * The scope is the server part of a load: the declared scope filters, the
 * server search and the sort. The bar shows one control for each scope filter,
 * chosen by how the filter matches. An exact, contains or prefix filter takes
 * values as chips: a value is typed and added with Enter, and a reference
 * takes its id until labels resolve. A range takes a minimum and a maximum,
 * each inclusive. An is-null filter keeps any row, the empty rows, or the rows
 * with a value. A band is a required range: until the operator sets its start,
 * the server reads its default days, and the bar says so. Its reset returns to
 * that default, and nothing removes it.
 *
 * A server search, when the read declares one, is one text that a declared
 * field contains, applied with Enter. A change calls back with the whole
 * scope, and the source starts a new load. The bar applies no filter in the
 * table, and the header keeps the refine filters, so the two layers read apart.
 */

import { X } from "lucide-solid";
import { createUniqueId, For, type JSX, Match, Show, Switch } from "solid-js";

import { Badge } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { Input } from "../components/ui/input";

/** How a scope filter matches: exactly when none is stated. */
export type DataTableScopeMatch = "contains" | "prefix" | "range" | "is_null";

/** The bounds of a range scope filter, as its controls hold them. Empty means unbounded. */
export interface DataTableScopeRange {
  readonly min: string;
  readonly max: string;
}

/**
 * One scope filter. A list filter keeps the rows whose field matches one of
 * the values. A range keeps the rows between its bounds. An is-null filter
 * keeps the empty rows when `empty` is true, and the rows with a value when it
 * is false.
 */
export interface DataTableScopeFilter<Field extends string = string> {
  readonly field: Field;
  readonly values: readonly string[];
  readonly range?: DataTableScopeRange;
  readonly empty?: boolean;
}

/** How one declared scope filter matches, and its band default. */
export interface DataTableScopeMode {
  readonly match?: DataTableScopeMatch | undefined;
  /** The contract type of a range's bounds. */
  readonly type?: string | undefined;
  readonly required?: boolean | undefined;
  readonly defaultLastDays?: number | undefined;
}

/** True when a scope filter narrows the rows. A band needs its start. */
export function scopeApplies(filter: DataTableScopeFilter, mode: DataTableScopeMode = {}): boolean {
  if (filter.empty !== undefined) {
    return true;
  }
  if (filter.range !== undefined) {
    return mode.required === true
      ? filter.range.min !== ""
      : filter.range.min !== "" || filter.range.max !== "";
  }
  return filter.values.length > 0;
}

const PLACEHOLDER: Record<string, string> = {
  exact: "add a value",
  contains: "add a part",
  prefix: "add a start",
};

/** The value chips of one list filter, and the input that adds a value. */
function ListControl(props: {
  filter: { readonly field: string; readonly label: string; readonly values: readonly string[] };
  match: string;
  id: string;
  onChange: (values: readonly string[]) => void;
}): JSX.Element {
  return (
    <>
      <For each={props.filter.values}>
        {(value) => (
          <Badge variant="secondary" class="gap-1">
            {value}
            <Button
              type="button"
              variant="ghost"
              size="icon-xs"
              aria-label={`remove ${props.filter.label} ${value}`}
              onClick={() => props.onChange(props.filter.values.filter((kept) => kept !== value))}
            >
              <X aria-hidden="true" />
            </Button>
          </Badge>
        )}
      </For>
      <Input
        id={props.id}
        class="h-7 w-36"
        placeholder={PLACEHOLDER[props.match]}
        onKeyDown={(event) => {
          if (event.key !== "Enter") {
            return;
          }
          const value = event.currentTarget.value.trim();
          event.currentTarget.value = "";
          if (value !== "" && !props.filter.values.includes(value)) {
            props.onChange([...props.filter.values, value]);
          }
        }}
      />
    </>
  );
}

/** The two bounds of one range, and for a band its default and its reset. */
function RangeControl(props: {
  label: string;
  id: string;
  mode: DataTableScopeMode;
  range: DataTableScopeRange;
  onChange: (range: DataTableScopeRange | undefined) => void;
}): JSX.Element {
  const time = () => props.mode.type === "timestamptz";
  const band = () => props.mode.required === true;
  const set = (range: DataTableScopeRange) =>
    props.onChange(range.min === "" && range.max === "" ? undefined : range);
  return (
    <>
      <Input
        id={props.id}
        class="h-7 w-48"
        type={time() ? "datetime-local" : "number"}
        aria-label={`${props.label} ${time() ? "from" : "min"}`}
        value={props.range.min}
        onChange={(event) => set({ ...props.range, min: event.currentTarget.value })}
      />
      <Input
        class="h-7 w-48"
        type={time() ? "datetime-local" : "number"}
        aria-label={`${props.label} ${time() ? "to" : "max"}`}
        value={props.range.max}
        onChange={(event) => set({ ...props.range, max: event.currentTarget.value })}
      />
      <Show
        when={band()}
        fallback={
          <Show when={props.range.min !== "" || props.range.max !== ""}>
            <Button
              type="button"
              variant="ghost"
              size="icon-xs"
              aria-label={`clear ${props.label}`}
              onClick={() => props.onChange(undefined)}
            >
              <X aria-hidden="true" />
            </Button>
          </Show>
        }
      >
        <Show
          when={props.range.min !== ""}
          fallback={
            <span data-slot="data-table-scope-band" class="text-xs text-muted-foreground">
              last {props.mode.defaultLastDays} days
            </span>
          }
        >
          <Button type="button" variant="ghost" size="sm" onClick={() => props.onChange(undefined)}>
            reset to last {props.mode.defaultLastDays} days
          </Button>
        </Show>
      </Show>
    </>
  );
}

/** The three choices of an is-null filter. */
function EmptyControl(props: {
  label: string;
  empty: boolean | undefined;
  onChange: (empty: boolean | undefined) => void;
}): JSX.Element {
  const choices: readonly { readonly text: string; readonly value: boolean | undefined }[] = [
    { text: "any", value: undefined },
    { text: "is empty", value: true },
    { text: "has a value", value: false },
  ];
  return (
    <div role="group" aria-label={props.label} class="flex gap-1">
      <For each={choices}>
        {(choice) => (
          <Button
            type="button"
            size="sm"
            variant={props.empty === choice.value ? "default" : "outline"}
            aria-pressed={props.empty === choice.value}
            onClick={() => props.onChange(choice.value)}
          >
            {choice.text}
          </Button>
        )}
      </For>
    </div>
  );
}

export function ScopeBar(props: {
  /** Each declared scope filter, with its label, its mode and its current value. */
  filters: readonly (DataTableScopeFilter & { readonly label: string; readonly mode: DataTableScopeMode })[];
  /** The server search, when the read declares one: its current text. */
  find?: string | undefined;
  /** The current sort, as each chip reads. */
  sort: readonly string[];
  onChange: (filter: DataTableScopeFilter) => void;
  onFind?: ((text: string) => void) | undefined;
}): JSX.Element {
  const findId = createUniqueId();
  return (
    <div data-slot="data-table-scope" class="flex shrink-0 flex-wrap items-center gap-4">
      <Show when={props.find !== undefined}>
        <div data-slot="data-table-scope-find" class="flex items-center gap-2">
          <label for={findId} class="text-xs font-medium uppercase">
            server search
          </label>
          <Input
            id={findId}
            type="search"
            class="h-7 w-48"
            value={props.find}
            onKeyDown={(event) => {
              if (event.key === "Enter") {
                props.onFind?.(event.currentTarget.value.trim());
              }
            }}
          />
        </div>
      </Show>
      <For each={props.filters}>
        {(filter) => {
          const id = createUniqueId();
          const match = () => filter.mode.match ?? "exact";
          return (
            <div data-field={filter.field} data-match={match()} class="flex flex-wrap items-center gap-2">
              <label for={id} class="text-xs font-medium uppercase">
                {filter.label}
              </label>
              <Switch
                fallback={
                  <ListControl
                    filter={filter}
                    match={match()}
                    id={id}
                    onChange={(values) => props.onChange({ field: filter.field, values })}
                  />
                }
              >
                <Match when={match() === "range"}>
                  <RangeControl
                    label={filter.label}
                    id={id}
                    mode={filter.mode}
                    range={filter.range ?? { min: "", max: "" }}
                    onChange={(range) =>
                      props.onChange(
                        range === undefined
                          ? { field: filter.field, values: [] }
                          : { field: filter.field, values: [], range },
                      )
                    }
                  />
                </Match>
                <Match when={match() === "is_null"}>
                  <EmptyControl
                    label={filter.label}
                    empty={filter.empty}
                    onChange={(empty) =>
                      props.onChange(
                        empty === undefined
                          ? { field: filter.field, values: [] }
                          : { field: filter.field, values: [], empty },
                      )
                    }
                  />
                </Match>
              </Switch>
            </div>
          );
        }}
      </For>
      <Show when={props.sort.length > 0}>
        <div data-slot="data-table-scope-sort" class="flex items-center gap-2">
          <span class="text-xs font-medium uppercase">sort</span>
          <For each={props.sort}>{(sort) => <Badge variant="outline">{sort}</Badge>}</For>
        </div>
      </Show>
    </div>
  );
}
