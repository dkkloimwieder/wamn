/**
 * The header of one declared column (wamn-9v2r.1): its label, a sort button
 * when the column can sort, and its menu.
 *
 * The menu sorts the column ascending or descending, hides it, pins it to the
 * start or the end, and unpins it. The arrangement works in every mode. A set
 * table adds its refine filter beside the label, and the aggregate choice to
 * the menu.
 */

import type { Column } from "@tanstack/solid-table";
import ArrowDown from "lucide-solid/icons/arrow-down";
import ArrowUp from "lucide-solid/icons/arrow-up";
import EllipsisVertical from "lucide-solid/icons/ellipsis-vertical";
import { For, type JSX, Match, Show, Switch } from "solid-js";

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
import type { Aggregate } from "./aggregate";
import type { GridFeatures } from "./grid";

/** What a header reads and changes of its column, whatever the table's features. */
export type HeaderColumn = Pick<
  Column<GridFeatures, object, unknown>,
  | "getCanSort"
  | "getIsSorted"
  | "getToggleSortingHandler"
  | "toggleSorting"
  | "toggleVisibility"
  | "getIsPinned"
  | "pin"
>;

/** The aggregate a column shows, the ones it allows, and the choice of one. */
export interface HeaderAggregate {
  readonly allowed: readonly Aggregate[];
  readonly chosen: Aggregate;
  readonly onChoose: (aggregate: Aggregate) => void;
}

export function ColumnHeader(props: {
  column: HeaderColumn;
  label: string;
  /** The refine filter of a set table. */
  filter?: JSX.Element | undefined;
  aggregate?: HeaderAggregate | undefined;
}): JSX.Element {
  const pinned = () => props.column.getIsPinned();
  return (
    <div class="flex items-center gap-1">
      <Show when={props.column.getCanSort()} fallback={props.label}>
        <Button
          type="button"
          variant="ghost"
          size="sm"
          class="-ms-2"
          onClick={(event: MouseEvent) => props.column.getToggleSortingHandler()?.(event)}
        >
          {props.label}
          <Switch>
            <Match when={props.column.getIsSorted() === "asc"}>
              <ArrowUp aria-hidden="true" />
            </Match>
            <Match when={props.column.getIsSorted() === "desc"}>
              <ArrowDown aria-hidden="true" />
            </Match>
          </Switch>
        </Button>
      </Show>
      {props.filter}
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
          <DropdownMenuItem
            disabled={!props.column.getCanSort()}
            onSelect={() => props.column.toggleSorting(false, false)}
          >
            sort ascending
          </DropdownMenuItem>
          <DropdownMenuItem
            disabled={!props.column.getCanSort()}
            onSelect={() => props.column.toggleSorting(true, false)}
          >
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
          <Show when={props.aggregate !== undefined && props.aggregate.allowed.length > 1 && props.aggregate}>
            {(aggregate) => (
              <>
                <DropdownMenuSeparator />
                <DropdownMenuGroup>
                  <DropdownMenuLabel>aggregate</DropdownMenuLabel>
                  <DropdownMenuRadioGroup
                    value={aggregate().chosen}
                    onChange={(value) => aggregate().onChoose(value as Aggregate)}
                  >
                    <For each={aggregate().allowed}>
                      {(choice) => <DropdownMenuRadioItem value={choice}>{choice}</DropdownMenuRadioItem>}
                    </For>
                  </DropdownMenuRadioGroup>
                </DropdownMenuGroup>
              </>
            )}
          </Show>
        </DropdownMenuContent>
      </DropdownMenu>
    </div>
  );
}
