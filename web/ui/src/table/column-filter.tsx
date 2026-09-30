/**
 * The refine filter of one table column: its value, the rows it keeps, the
 * text of its chip, and the control in the column header.
 *
 * The column type chooses the filter. Text contains a value, in any case. A
 * number is between a minimum and a maximum. A time is between a start and an
 * end. A boolean is true or false. An id and an int64 equal a value. Every
 * type can also be empty or not empty, and json and bytes can only be that.
 *
 * A filter runs in the table, which shows it only on a fully read set.
 */

import type { Column } from "@tanstack/solid-table";
import ListFilter from "lucide-solid/icons/list-filter";
import { createUniqueId, For, type JSX, Match, Switch } from "solid-js";

import { Button } from "../components/ui/button";
import { Field, FieldLabel } from "../components/ui/field";
import { Input } from "../components/ui/input";
import { Popover, PopoverContent, PopoverTitle, PopoverTrigger } from "../components/ui/popover";
import { isEmpty, type TableColumnType } from "./columns";
import type { SetFeatures } from "./set-table";

/** The filter of one column, as the table state holds it. */
export type SetFilter =
  | { readonly type: "contains"; readonly text: string }
  | { readonly type: "range"; readonly min: string; readonly max: string }
  | { readonly type: "is"; readonly value: boolean }
  | { readonly type: "equals"; readonly value: string }
  | { readonly type: "empty" }
  | { readonly type: "not-empty" };

/** The value filter a column type offers, beside empty and not empty. */
type ValueFilter = "contains" | "number" | "time" | "is" | "equals" | null;

function valueFilter(type: TableColumnType): ValueFilter {
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

/** True when a row value of one column type passes the filter. */
export function filterMatches(type: TableColumnType, value: unknown, filter: SetFilter): boolean {
  switch (filter.type) {
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
      return (filter.min === "" || at >= read(filter.min)) && (filter.max === "" || at <= read(filter.max));
    }
  }
}

/** The text of a filter's chip, after the column label. */
export function filterText(type: TableColumnType, filter: SetFilter): string {
  switch (filter.type) {
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

const isValue = (filter: SetFilter | undefined, value: boolean) => filter?.type === "is" && filter.value === value;

/** One labeled input of a filter control. */
function FilterInput(props: {
  label: string;
  type: "text" | "number" | "datetime-local";
  value: string;
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
        onInput={(event) => props.onInput(event.currentTarget.value)}
      />
    </Field>
  );
}

/** The filter control in one column header. */
export function ColumnFilter<TRow extends object>(props: {
  column: Column<SetFeatures, TRow, unknown>;
  type: TableColumnType;
  label: string;
}): JSX.Element {
  const current = () => props.column.getFilterValue() as SetFilter | undefined;
  const set = (filter: SetFilter | undefined) => props.column.setFilterValue(filter);
  const range = () => {
    const filter = current();
    return filter?.type === "range" ? filter : { type: "range" as const, min: "", max: "" };
  };
  const setRange = (min: string, max: string) =>
    set(min === "" && max === "" ? undefined : { type: "range", min, max });
  const text = (type: "contains" | "equals") => {
    const filter = current();
    return filter?.type === "contains" && type === "contains"
      ? filter.text
      : filter?.type === "equals" && type === "equals"
        ? filter.value
        : "";
  };
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
        <Switch>
          <Match when={valueFilter(props.type) === "contains"}>
            <FilterInput
              label="contains"
              type="text"
              value={text("contains")}
              onInput={(value) => set(value === "" ? undefined : { type: "contains", text: value })}
            />
          </Match>
          <Match when={valueFilter(props.type) === "equals"}>
            <FilterInput
              label="equals"
              type="text"
              value={text("equals")}
              onInput={(value) => set(value === "" ? undefined : { type: "equals", value })}
            />
          </Match>
          <Match when={valueFilter(props.type) === "number" || valueFilter(props.type) === "time"}>
            <div class="grid grid-cols-2 gap-2">
              <FilterInput
                label={valueFilter(props.type) === "time" ? "from" : "min"}
                type={valueFilter(props.type) === "time" ? "datetime-local" : "number"}
                value={range().min}
                onInput={(value) => setRange(value, range().max)}
              />
              <FilterInput
                label={valueFilter(props.type) === "time" ? "to" : "max"}
                type={valueFilter(props.type) === "time" ? "datetime-local" : "number"}
                value={range().max}
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
                    onClick={() => set({ type: "is", value })}
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
            variant={current()?.type === "empty" ? "default" : "outline"}
            onClick={() => set({ type: "empty" })}
          >
            is empty
          </Button>
          <Button
            type="button"
            size="sm"
            variant={current()?.type === "not-empty" ? "default" : "outline"}
            onClick={() => set({ type: "not-empty" })}
          >
            is not empty
          </Button>
          <Button
            type="button"
            size="sm"
            variant="ghost"
            disabled={current() === undefined}
            onClick={() => set(undefined)}
          >
            clear
          </Button>
        </div>
      </PopoverContent>
    </Popover>
  );
}
