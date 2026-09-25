/**
 * The refine filter of one table column: its value, the rows it keeps, the
 * text of its chip, and the control in the column header.
 *
 * The column type chooses the filter. Text contains a value, in any case. A
 * number is between a minimum and a maximum. A time is between a start and an
 * end. A boolean is true or false. An id and an int64 equal a value. Every
 * type can also be empty or not empty, and json and bytes can only be that.
 *
 * A filter runs in the table, so it applies only to a fully read set. On a set
 * that is not fully read, the control says why and changes nothing.
 */

import type { Column } from "@tanstack/solid-table";
import { ListFilter } from "lucide-solid";
import { createUniqueId, For, type JSX, Match, Show, Switch } from "solid-js";

import { Button } from "../components/ui/button";
import { Field, FieldLabel } from "../components/ui/field";
import { Input } from "../components/ui/input";
import { Popover, PopoverContent, PopoverTitle, PopoverTrigger } from "../components/ui/popover";
import type { DataTableColumnType, DataTableFeatures } from "./data-table";

/** The filter of one column, as the table state holds it. */
export type DataTableFilter =
  | { readonly kind: "contains"; readonly text: string }
  | { readonly kind: "range"; readonly min: string; readonly max: string }
  | { readonly kind: "is"; readonly value: boolean }
  | { readonly kind: "equals"; readonly value: string }
  | { readonly kind: "empty" }
  | { readonly kind: "not-empty" };

/** The value filter a column type offers, beside empty and not empty. */
type ValueFilter = "contains" | "number" | "time" | "is" | "equals" | null;

function valueFilter(type: DataTableColumnType): ValueFilter {
  switch (type) {
    case "text":
      return "contains";
    case "int32":
    case "float64":
    case "numeric":
      return "number";
    case "timestamptz":
      return "time";
    case "boolean":
      return "is";
    case "uuid":
    case "int64":
      return "equals";
    case "json":
    case "bytes":
      return null;
  }
}

/** The text a filter control shows when the set is not fully read. */
export const FILTER_NEEDS_FULL_SET =
  "Filters apply only to a fully read set. Raise the cap to read every row.";

const isEmpty = (value: unknown) => value === null || value === undefined || value === "";

/** True when a row value of one column type passes the filter. */
export function filterMatches(
  type: DataTableColumnType,
  value: unknown,
  filter: DataTableFilter,
): boolean {
  switch (filter.kind) {
    case "empty":
      return isEmpty(value);
    case "not-empty":
      return !isEmpty(value);
    case "contains":
      return !isEmpty(value) && String(value).toLowerCase().includes(filter.text.toLowerCase());
    case "is":
      return value === filter.value;
    case "equals":
      return !isEmpty(value) && String(value).toLowerCase() === filter.value.trim().toLowerCase();
    case "range": {
      if (isEmpty(value)) {
        return false;
      }
      // A time compares as an instant; the control gives a local time.
      const read = type === "timestamptz" ? Date.parse : Number;
      const at = read(String(value));
      return (
        (filter.min === "" || at >= read(filter.min)) && (filter.max === "" || at <= read(filter.max))
      );
    }
  }
}

/** The text of a filter's chip, after the column label. */
export function filterText(type: DataTableColumnType, filter: DataTableFilter): string {
  switch (filter.kind) {
    case "empty":
      return "is empty";
    case "not-empty":
      return "is not empty";
    case "contains":
      return `contains "${filter.text}"`;
    case "is":
      return `is ${String(filter.value)}`;
    case "equals":
      return `equals ${filter.value}`;
    case "range": {
      const [from, to] = type === "timestamptz" ? ["from", "to"] : ["at least", "at most"];
      if (filter.min !== "" && filter.max !== "") {
        return `${filter.min} to ${filter.max}`;
      }
      return filter.min !== "" ? `${from} ${filter.min}` : `${to} ${filter.max}`;
    }
  }
}

const isValue = (filter: DataTableFilter | undefined, value: boolean) =>
  filter?.kind === "is" && filter.value === value;

/** One labeled input of a filter control. */
function FilterInput(props: {
  label: string;
  type: "text" | "number" | "datetime-local";
  value: string;
  disabled: boolean;
  onInput: (value: string) => void;
}): JSX.Element {
  const id = createUniqueId();
  return (
    <Field>
      <FieldLabel for={id}>{props.label}</FieldLabel>
      <Input
        id={id}
        type={props.type}
        value={props.value}
        disabled={props.disabled}
        onInput={(event) => props.onInput(event.currentTarget.value)}
      />
    </Field>
  );
}

/** The filter control in one column header. */
export function ColumnFilter<TRow extends object>(props: {
  column: Column<DataTableFeatures, TRow, unknown>;
  type: DataTableColumnType;
  label: string;
  /** False when the set is not fully read. */
  enabled: boolean;
}): JSX.Element {
  const current = () => props.column.getFilterValue() as DataTableFilter | undefined;
  const set = (filter: DataTableFilter | undefined) => props.column.setFilterValue(filter);
  const range = () => {
    const filter = current();
    return filter?.kind === "range" ? filter : { kind: "range" as const, min: "", max: "" };
  };
  const setRange = (min: string, max: string) =>
    set(min === "" && max === "" ? undefined : { kind: "range", min, max });
  const text = (kind: "contains" | "equals") => {
    const filter = current();
    return filter?.kind === "contains" && kind === "contains"
      ? filter.text
      : filter?.kind === "equals" && kind === "equals"
        ? filter.value
        : "";
  };
  const disabled = () => !props.enabled;

  return (
    <Popover>
      <PopoverTrigger
        as={Button}
        variant={props.column.getIsFiltered() ? "secondary" : "ghost"}
        size="icon-xs"
        aria-label={`filter ${props.label}`}
      >
        <ListFilter aria-hidden="true" />
      </PopoverTrigger>
      <PopoverContent class="flex flex-col gap-3">
        <PopoverTitle>filter {props.label}</PopoverTitle>
        <Show when={disabled()}>
          <p class="text-sm text-muted-foreground">{FILTER_NEEDS_FULL_SET}</p>
        </Show>
        <Switch>
          <Match when={valueFilter(props.type) === "contains"}>
            <FilterInput
              label="contains"
              type="text"
              value={text("contains")}
              disabled={disabled()}
              onInput={(value) => set(value === "" ? undefined : { kind: "contains", text: value })}
            />
          </Match>
          <Match when={valueFilter(props.type) === "equals"}>
            <FilterInput
              label="equals"
              type="text"
              value={text("equals")}
              disabled={disabled()}
              onInput={(value) => set(value === "" ? undefined : { kind: "equals", value })}
            />
          </Match>
          <Match when={valueFilter(props.type) === "number" || valueFilter(props.type) === "time"}>
            <div class="grid grid-cols-2 gap-2">
              <FilterInput
                label={valueFilter(props.type) === "time" ? "from" : "min"}
                type={valueFilter(props.type) === "time" ? "datetime-local" : "number"}
                value={range().min}
                disabled={disabled()}
                onInput={(value) => setRange(value, range().max)}
              />
              <FilterInput
                label={valueFilter(props.type) === "time" ? "to" : "max"}
                type={valueFilter(props.type) === "time" ? "datetime-local" : "number"}
                value={range().max}
                disabled={disabled()}
                onInput={(value) => setRange(range().min, value)}
              />
            </div>
          </Match>
          <Match when={valueFilter(props.type) === "is"}>
            <div class="flex gap-2">
              <For each={[true, false]}>
                {(value) => (
                  <Button
                    type="button"
                    size="sm"
                    variant={isValue(current(), value) ? "default" : "outline"}
                    disabled={disabled()}
                    onClick={() => set({ kind: "is", value })}
                  >
                    {String(value)}
                  </Button>
                )}
              </For>
            </div>
          </Match>
        </Switch>
        <div class="flex flex-wrap gap-2">
          <Button
            type="button"
            size="sm"
            variant={current()?.kind === "empty" ? "default" : "outline"}
            disabled={disabled()}
            onClick={() => set({ kind: "empty" })}
          >
            is empty
          </Button>
          <Button
            type="button"
            size="sm"
            variant={current()?.kind === "not-empty" ? "default" : "outline"}
            disabled={disabled()}
            onClick={() => set({ kind: "not-empty" })}
          >
            is not empty
          </Button>
          <Button
            type="button"
            size="sm"
            variant="ghost"
            disabled={disabled() || current() === undefined}
            onClick={() => set(undefined)}
          >
            clear
          </Button>
        </div>
      </PopoverContent>
    </Popover>
  );
}
