/**
 * The header menu of one DataTable column (wamn-9v2r.1).
 *
 * It sorts the column ascending or descending, hides it, pins it to the start
 * or the end, unpins it, and chooses its aggregate from the ones its type
 * allows. The column arrangement works in every mode. A sort follows the
 * header click's rule: the table sorts a fully read set, and asks for a new
 * load otherwise.
 */

import type { Column } from "@tanstack/solid-table";
import { EllipsisVertical } from "lucide-solid";
import { For, type JSX, Show } from "solid-js";

import { Button } from "../components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "../components/ui/dropdown-menu";
import type { DataTableAggregate } from "./aggregate";
import type { DataTableFeatures } from "./data-table";

export function ColumnMenu<TRow extends object>(props: {
  column: Column<DataTableFeatures, TRow, unknown>;
  label: string;
  /** Called after the menu changes the sort. */
  onSort: () => void;
  /** The aggregates the column allows, and the one it shows. */
  aggregates: readonly DataTableAggregate[];
  aggregate: DataTableAggregate;
  onAggregate: (aggregate: DataTableAggregate) => void;
}): JSX.Element {
  const sort = (descending: boolean) => {
    props.column.toggleSorting(descending, false);
    props.onSort();
  };
  const pinned = () => props.column.getIsPinned();
  return (
    <DropdownMenu>
      <DropdownMenuTrigger
        as={Button}
        type="button"
        variant="ghost"
        size="icon-xs"
        aria-label={`menu ${props.label}`}
      >
        <EllipsisVertical aria-hidden="true" />
      </DropdownMenuTrigger>
      <DropdownMenuContent class="w-44">
        <DropdownMenuItem disabled={!props.column.getCanSort()} onSelect={() => sort(false)}>
          sort ascending
        </DropdownMenuItem>
        <DropdownMenuItem disabled={!props.column.getCanSort()} onSelect={() => sort(true)}>
          sort descending
        </DropdownMenuItem>
        <DropdownMenuSeparator />
        <DropdownMenuItem onSelect={() => props.column.toggleVisibility(false)}>hide</DropdownMenuItem>
        <Show when={pinned() !== "start"}>
          <DropdownMenuItem onSelect={() => props.column.pin("start")}>pin left</DropdownMenuItem>
        </Show>
        <Show when={pinned() !== "end"}>
          <DropdownMenuItem onSelect={() => props.column.pin("end")}>pin right</DropdownMenuItem>
        </Show>
        <Show when={pinned() !== false}>
          <DropdownMenuItem onSelect={() => props.column.pin(false)}>unpin</DropdownMenuItem>
        </Show>
        <Show when={props.aggregates.length > 1}>
          <DropdownMenuSeparator />
          <DropdownMenuGroup>
            <DropdownMenuLabel>aggregate</DropdownMenuLabel>
            <DropdownMenuRadioGroup
              value={props.aggregate}
              onChange={(value) => props.onAggregate(value as DataTableAggregate)}
            >
              <For each={props.aggregates}>
                {(aggregate) => <DropdownMenuRadioItem value={aggregate}>{aggregate}</DropdownMenuRadioItem>}
              </For>
            </DropdownMenuRadioGroup>
          </DropdownMenuGroup>
        </Show>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
