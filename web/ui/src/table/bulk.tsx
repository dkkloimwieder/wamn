/**
 * Bulk actions of the DataTable (wamn-8iul.3).
 *
 * The first column selects rows. Its header selects every loaded row, and only
 * the loaded rows: a set that is not fully read has rows the table never read,
 * and the bar says so. The bar offers each action that takes many rows. One
 * submit hands the selected rows to the caller, which sends one call with one
 * outer input for each row, and the release runs each input on its own. The
 * caller returns one result for each row, and each row shows its own result
 * beside its checkbox, so a refusal marks only its row.
 */

import type { ColumnDef } from "@tanstack/solid-table";
import { createSignal, createUniqueId, type JSX, Show } from "solid-js";

import { Badge } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { Checkbox } from "../components/ui/checkbox";
import { Field, FieldDescription } from "../components/ui/field";
import { ChoiceField } from "../fields";
import type { DataTableFeatures } from "./data-table";

/** One operation that a row opens, as the table definition names it. */
export interface DataTableAction {
  /** The canonical identity of the operation. */
  readonly operation: string;
  readonly label: string;
}

/** What one row's input of a write came to. */
export interface DataTableRowResult {
  readonly status: "completed" | "refused" | "uncertain";
  /** The refusal code or the reason, which the row shows. */
  readonly message?: string | undefined;
}

/** The text beside select all on a set that is not fully read. */
export const SELECT_LOADED_ONLY = "Select all takes the loaded rows only. Full dataset cannot be loaded.";

/** The id of the first column, which selects rows. */
export const SELECT_COLUMN = "rowSelect";

/** The first column: a checkbox and the last result of each data row. */
export function selectColumn<TRow extends object>(
  result: (id: string) => DataTableRowResult | undefined,
): ColumnDef<DataTableFeatures, TRow> {
  return {
    id: SELECT_COLUMN,
    header: (context) => {
      const id = createUniqueId();
      const table = context.table;
      return (
        <div class="flex items-center">
          <label for={id} class="sr-only">
            select all loaded rows
          </label>
          <Checkbox
            id={id}
            checked={table.getIsAllRowsSelected()}
            indeterminate={table.getIsSomeRowsSelected()}
            onChange={(checked: boolean) => table.toggleAllRowsSelected(checked)}
          />
        </div>
      );
    },
    cell: (context) => {
      const id = createUniqueId();
      const row = context.row;
      return (
        <Show when={!row.getIsGrouped()}>
          <div class="flex items-center gap-2">
            <label for={id} class="sr-only">
              select row {row.id}
            </label>
            <Checkbox
              id={id}
              checked={row.getIsSelected()}
              onChange={(checked: boolean) => row.toggleSelected(checked)}
            />
            <Show when={result(row.id)}>
              {(shown) => (
                <Badge
                  data-slot="data-table-row-result"
                  data-status={shown().status}
                  variant={shown().status === "completed" ? "secondary" : "destructive"}
                >
                  {shown().status === "completed" ? "done" : `${shown().status}: ${shown().message ?? ""}`}
                </Badge>
              )}
            </Show>
          </div>
        </Show>
      );
    },
    size: 150,
    enableSorting: false,
    enableColumnFilter: false,
    enableGlobalFilter: false,
    enableGrouping: false,
    enableHiding: false,
    enablePinning: false,
    enableResizing: false,
  };
}

/** The bar that runs one action over the selected rows. */
export function BulkBar(props: {
  readonly actions: readonly DataTableAction[];
  /** How many rows are selected. */
  readonly selected: number;
  readonly fullyRead: boolean;
  /** Runs the action over the selected rows, once. */
  readonly onRun: (operation: string) => Promise<void>;
}): JSX.Element {
  const [chosen, setChosen] = createSignal<string>("");
  const [running, setRunning] = createSignal(false);
  return (
    <div data-slot="data-table-bulk" class="flex shrink-0 flex-wrap items-end gap-2">
      <div class="w-48">
        <ChoiceField
          label="bulk action"
          choices={props.actions.map((action) => ({ value: action.operation, text: action.label }))}
          allowEmpty={false}
          value={chosen()}
          onChange={setChosen}
        />
      </div>
      <Field class="w-auto">
        <Button
          type="button"
          disabled={props.selected === 0 || chosen() === "" || running()}
          onClick={async () => {
            setRunning(true);
            try {
              await props.onRun(chosen());
            } finally {
              setRunning(false);
            }
          }}
        >
          run on {props.selected} selected
        </Button>
        <Show when={!props.fullyRead}>
          <FieldDescription>{SELECT_LOADED_ONLY}</FieldDescription>
        </Show>
      </Field>
    </div>
  );
}
