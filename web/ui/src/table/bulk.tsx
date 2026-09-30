/**
 * Bulk actions of a QueryTable (wamn-8iul.3).
 *
 * The first column selects rows. Its header selects every loaded row, and only
 * the loaded rows: a set that is not fully read has rows the table never read,
 * and the bar says so. The selection is the QueryTable's, so it holds whether
 * or not a set table shows the rows. The bar offers each action that takes
 * many rows. One submit hands the selected rows to the caller, which sends one
 * call with one outer input for each row, and the release runs each input on
 * its own. The caller returns one result for each row, and each row shows its
 * own result beside its checkbox, so a refusal marks only its row.
 */

import { createMemo, createSignal, createUniqueId, type JSX, lazy, Show } from "solid-js";
import { Dynamic } from "solid-js/web";

import { fillMember, type JsonValue, type Outcome, type Transport, writeMember } from "@wamn/web-runtime";

import { Badge } from "../components/ui/badge";
import { Button } from "../components/ui/button";
import { Checkbox } from "../components/ui/checkbox";
import { Field, FieldDescription } from "../components/ui/field";
import { Sheet, SheetContent, SheetHeader, SheetTitle } from "../components/ui/sheet";
import { ChoiceField } from "../fields";
import type { BuiltColumn } from "./columns";
import { writeOutcome } from "./edit-cell";
import type { QueryTableAction, QueryTableDefinition } from "./query-definition";
import type { QueryLoad } from "./table-load";

/** One operation that a row opens, as the table definition names it. */
export interface BulkAction {
  /** The canonical identity of the operation. */
  readonly operation: string;
  readonly label: string;
}

/** What one row's input of a write came to. */
export interface RowResult {
  readonly status: "completed" | "refused" | "uncertain";
  /** The refusal code or the reason, which the row shows. */
  readonly message?: string | undefined;
}

/** The text beside select all on a set that is not fully read. */
export const SELECT_LOADED_ONLY = "Select all takes the loaded rows only. Full dataset cannot be loaded.";

/** The id of the first column, which selects rows. */
export const SELECT_COLUMN = "rowSelect";

/** The rows a table selects, by row id, over the ids of its loaded rows. */
export function createSelection(loaded: () => readonly string[]) {
  const [selected, setSelected] = createSignal<ReadonlySet<string>>(new Set());
  const isSelected = (id: string) => selected().has(id);
  const toggle = (id: string, checked: boolean) =>
    setSelected((current) => {
      const next = new Set(current);
      if (checked) {
        next.add(id);
      } else {
        next.delete(id);
      }
      return next;
    });
  const toggleAll = (checked: boolean) => setSelected(new Set(checked ? loaded() : []));
  return { selected, isSelected, toggle, toggleAll };
}

export type Selection = ReturnType<typeof createSelection>;

/** The first column: a checkbox and the last result of each data row. */
export function selectColumn<TRow extends object>(
  selection: Selection,
  loaded: () => readonly string[],
  result: (id: string) => RowResult | undefined,
): BuiltColumn<TRow> {
  return {
    id: SELECT_COLUMN,
    size: 150,
    header: () => {
      const id = createUniqueId();
      const all = () => loaded().length > 0 && loaded().every(selection.isSelected);
      return (
        <div class="flex items-center">
          <label for={id} class="sr-only">
            select all loaded rows
          </label>
          <Checkbox
            id={id}
            checked={all()}
            indeterminate={!all() && loaded().some(selection.isSelected)}
            onChange={(checked: boolean) => selection.toggleAll(checked)}
          />
        </div>
      );
    },
    cell: (_row, rowId) => {
      const id = createUniqueId();
      return (
        <div class="flex items-center gap-2">
          <label for={id} class="sr-only">
            select row {rowId}
          </label>
          <Checkbox
            id={id}
            checked={selection.isSelected(rowId)}
            onChange={(checked: boolean) => selection.toggle(rowId, checked)}
          />
          <Show when={result(rowId)}>
            {(shown) => (
              <Badge
                data-slot="table-row-result"
                data-status={shown().status}
                variant={shown().status === "completed" ? "secondary" : "destructive"}
              >
                {shown().status === "completed" ? "done" : `${shown().status}: ${shown().message ?? ""}`}
              </Badge>
            )}
          </Show>
        </div>
      );
    },
  };
}

/** The bar that runs one action over the selected rows. */
export function BulkBar(props: {
  readonly actions: readonly BulkAction[];
  /** How many rows are selected. */
  readonly selected: number;
  readonly fullyRead: boolean;
  /** Runs the action over the selected rows, once. */
  readonly onRun: (operation: string) => Promise<void>;
}): JSX.Element {
  const [chosen, setChosen] = createSignal<string>("");
  const [running, setRunning] = createSignal(false);
  return (
    <div data-slot="table-bulk" class="flex shrink-0 flex-wrap items-end gap-2">
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
          // eslint-disable-next-line solid/reactivity -- onClick is an event handler, not a tracked scope.
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

type Member = { readonly [name: string]: unknown };

/** The values a row fills into the form an action opens. */
export const fillOf = (action: QueryTableAction, row: object) =>
  action.fill.reduce<object>(
    (initial, { field, input }) => fillMember(initial, input, (row as Member)[field] as JsonValue),
    {},
  );

/** What one outcome of a row's input in a bulk action means to that row. */
const rowResult = (outcome: Outcome<unknown>): RowResult =>
  outcome.status === "completed" ? { status: "completed" } : writeOutcome(outcome);

/** One bulk action the operator opened: its action, each row's values, and where its results go. */
interface BulkRun {
  readonly action: QueryTableAction;
  readonly rows: readonly object[];
  readonly finish: (results: readonly RowResult[]) => void;
}

/**
 * The bulk actions of a QueryTable: each action that takes many rows and names
 * its form, the selection, the first column, the bar and the sheet. A bulk
 * action needs a transport that sends many outer inputs in one call. A run
 * opens the action's form over the selected rows, in the loaded order, keeps
 * each row's result, and loads again.
 */
export function createBulk<TRow extends object>(options: {
  readonly definition: QueryTableDefinition<TRow>;
  readonly transport: Transport;
  readonly load: QueryLoad<TRow>;
  readonly idOf: (row: TRow) => string;
}) {
  const { load, idOf } = options;
  const actions =
    options.transport.invokeEach === undefined
      ? []
      : options.definition.actions.filter((action) => action.many && action.form !== undefined);
  const loaded = createMemo(() => load.state().rows.map(idOf));
  const selection = createSelection(loaded);
  /** The last result of each row a bulk action ran over, by row id. */
  const [results, setResults] = createSignal<Readonly<Record<string, RowResult>>>({});
  const [running, setRunning] = createSignal<BulkRun | null>(null);
  /** Each action's form, which loads when the action first opens. */
  const forms = new Map(actions.map((action) => [action, lazy(action.form!)]));
  const formOf = (action: QueryTableAction) => forms.get(action)!;
  const finish = (outcomes: readonly RowResult[]) => {
    const run = running();
    setRunning(null);
    run?.finish(outcomes);
  };

  async function run(operation: string) {
    const action = actions.find((candidate) => candidate.operation === operation)!;
    const selected = load.state().rows.filter((row) => selection.isSelected(idOf(row)));
    const outcomes = await new Promise<readonly RowResult[]>((done) => {
      const revision = action.revision;
      setRunning({
        action,
        rows: selected.map((row) => {
          const filled = fillOf(action, row);
          return revision === undefined
            ? filled
            : writeMember(filled, revision.input, (row as Member)[revision.field] as JsonValue);
        }),
        finish: done,
      });
    });
    // A closed sheet ran nothing, so no row shows a result.
    if (outcomes.length === 0) {
      return;
    }
    setResults((current) => ({
      ...current,
      ...Object.fromEntries(selected.map((row, index) => [idOf(row), outcomes[index]!])),
    }));
    load.reload();
  }

  const Bar = () => (
    <Show when={actions.length > 0}>
      <BulkBar
        actions={actions}
        selected={loaded().filter(selection.isSelected).length}
        fullyRead={!load.partial()}
        onRun={run}
      />
    </Show>
  );

  const Form = () => (
    <Sheet open={running() !== null} onOpenChange={(open) => !open && finish([])}>
      <SheetContent>
        <Show when={running()}>
          {(current) => (
            <div class="flex flex-col gap-4 overflow-y-auto p-4">
              <SheetHeader>
                <SheetTitle as="p">
                  {current().action.label} on {current().rows.length} rows
                </SheetTitle>
              </SheetHeader>
              <Dynamic
                component={formOf(current().action)}
                transport={options.transport}
                rows={current().rows}
                onEach={(outcomes: readonly Outcome<unknown>[]) => finish(outcomes.map(rowResult))}
              />
            </div>
          )}
        </Show>
      </SheetContent>
    </Sheet>
  );

  return {
    /** The first column, when a bulk action exists. */
    columns: (): BuiltColumn<TRow>[] =>
      // eslint-disable-next-line solid/reactivity -- an accessor that the table reads in JSX.
      actions.length === 0 ? [] : [selectColumn<TRow>(selection, loaded, (id) => results()[id])],
    Bar,
    Form,
  };
}
