/**
 * The column panel of the DataTable (wamn-9v2r.1).
 *
 * It lists every column in the table's order, each with a switch that shows or
 * hides it. A column moves by a drag onto another row or by its arrows. Show
 * all shows every column, and reset puts the columns back in the order of the
 * table definition. The panel changes only the table state.
 */

import { ArrowDown, ArrowUp, Columns3 } from "lucide-solid";
import { createSignal, createUniqueId, For, type JSX } from "solid-js";

import { Button } from "../components/ui/button";
import { Popover, PopoverContent, PopoverTitle, PopoverTrigger } from "../components/ui/popover";
import { Switch } from "../components/ui/switch";

export interface ColumnPanelColumn {
  readonly id: string;
  readonly label: string;
  readonly visible: boolean;
}

/** Moves one id to the place of another, in a copy of the order. */
export function moveColumn(order: readonly string[], id: string, to: number): string[] {
  const next = order.filter((candidate) => candidate !== id);
  next.splice(Math.max(0, Math.min(to, next.length)), 0, id);
  return next;
}

export function ColumnPanel(props: {
  /** Every column, in the table's order. */
  columns: readonly ColumnPanelColumn[];
  onVisible: (id: string, visible: boolean) => void;
  onOrder: (order: readonly string[]) => void;
  onShowAll: () => void;
  onReset: () => void;
}): JSX.Element {
  const [dragged, setDragged] = createSignal<string | null>(null);
  const order = () => props.columns.map((column) => column.id);
  const move = (id: string, to: number) => props.onOrder(moveColumn(order(), id, to));
  return (
    <Popover placement="bottom-end">
      <PopoverTrigger as={Button} type="button" variant="outline">
        <Columns3 aria-hidden="true" />
        columns
      </PopoverTrigger>
      <PopoverContent class="w-72">
        <PopoverTitle>columns</PopoverTitle>
        <ul data-slot="data-table-column-panel" class="flex flex-col gap-1">
          <For each={props.columns}>
            {(column, index) => {
              const id = createUniqueId();
              return (
                <li
                  data-column={column.id}
                  draggable="true"
                  class="flex items-center gap-2"
                  classList={{ "opacity-50": dragged() === column.id }}
                  onDragStart={(event) => {
                    setDragged(column.id);
                    event.dataTransfer?.setData("text/plain", column.id);
                  }}
                  onDragEnd={() => setDragged(null)}
                  onDragOver={(event) => event.preventDefault()}
                  onDrop={(event) => {
                    event.preventDefault();
                    const from = dragged();
                    if (from !== null && from !== column.id) {
                      move(from, index());
                    }
                    setDragged(null);
                  }}
                >
                  <Switch
                    id={id}
                    size="sm"
                    checked={column.visible}
                    onChange={(visible: boolean) => props.onVisible(column.id, visible)}
                  />
                  <label for={id} class="flex-1 truncate text-xs">
                    {column.label}
                  </label>
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon-xs"
                    aria-label={`move ${column.label} up`}
                    disabled={index() === 0}
                    onClick={() => move(column.id, index() - 1)}
                  >
                    <ArrowUp aria-hidden="true" />
                  </Button>
                  <Button
                    type="button"
                    variant="ghost"
                    size="icon-xs"
                    aria-label={`move ${column.label} down`}
                    disabled={index() === props.columns.length - 1}
                    onClick={() => move(column.id, index() + 1)}
                  >
                    <ArrowDown aria-hidden="true" />
                  </Button>
                </li>
              );
            }}
          </For>
        </ul>
        <div class="flex gap-2">
          <Button type="button" size="sm" variant="outline" onClick={() => props.onShowAll()}>
            show all
          </Button>
          <Button type="button" size="sm" variant="ghost" onClick={() => props.onReset()}>
            reset order
          </Button>
        </div>
      </PopoverContent>
    </Popover>
  );
}
