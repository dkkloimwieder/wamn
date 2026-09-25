/**
 * The scope bar of the DataTable (wamn-9v2r.1).
 *
 * The scope is the server part of a load: the declared scope filters, which
 * are the contract's IN filters, and the sort. The bar shows one control for
 * each scope filter, with its values as chips, and the current sort as a chip.
 * A value is typed and added with Enter. A reference takes its id until labels
 * resolve. A change calls back with every scope filter, and the source starts
 * a new load. The bar applies no filter in the table, and the header keeps the
 * refine filters, so the two layers read apart.
 */

import { X } from "lucide-solid";
import { createUniqueId, For, type JSX, Show } from "solid-js";

import { Badge } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { Input } from "../components/ui/input";

/** One scope filter: the rows whose field holds one of the values. */
export interface DataTableScopeFilter<Field extends string = string> {
  readonly field: Field;
  readonly values: readonly string[];
}

export function ScopeBar(props: {
  /** Each declared scope filter, with its label and its values. */
  filters: readonly { readonly field: string; readonly label: string; readonly values: readonly string[] }[];
  /** The current sort, as each chip reads. */
  sort: readonly string[];
  onChange: (field: string, values: readonly string[]) => void;
}): JSX.Element {
  return (
    <div data-slot="data-table-scope" class="flex shrink-0 flex-wrap items-center gap-4">
      <For each={props.filters}>
        {(filter) => {
          const id = createUniqueId();
          return (
            <div data-field={filter.field} class="flex flex-wrap items-center gap-2">
              <label for={id} class="text-xs font-medium uppercase">
                {filter.label}
              </label>
              <For each={filter.values}>
                {(value) => (
                  <Badge variant="secondary" class="gap-1">
                    {value}
                    <Button
                      type="button"
                      variant="ghost"
                      size="icon-xs"
                      aria-label={`remove ${filter.label} ${value}`}
                      onClick={() =>
                        props.onChange(
                          filter.field,
                          filter.values.filter((kept) => kept !== value),
                        )
                      }
                    >
                      <X aria-hidden="true" />
                    </Button>
                  </Badge>
                )}
              </For>
              <Input
                id={id}
                class="h-7 w-36"
                placeholder="add a value"
                onKeyDown={(event) => {
                  if (event.key !== "Enter") {
                    return;
                  }
                  const value = event.currentTarget.value.trim();
                  event.currentTarget.value = "";
                  if (value !== "" && !filter.values.includes(value)) {
                    props.onChange(filter.field, [...filter.values, value]);
                  }
                }}
              />
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
