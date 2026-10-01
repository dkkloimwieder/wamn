/**
 * The table of one generated read, wired from its table definition alone
 * (wamn-sa7d.1, wamn-5pzt).
 *
 * The emitter writes the definition as data, and this component does the
 * rest, so no screen carries table code. It is the server table: everything
 * that talks to the read.
 *
 * - The read loads when the table mounts, and again after every write on the
 *   transport that the table did not send itself (`table-load.ts`).
 * - The scope bar holds the declared scope filters, with the control each
 *   match mode takes, and the server search when the read declares one. A
 *   filter that the caller fixes is not offered. A scope change, a cap change
 *   and a refresh each start a new load.
 * - A row edits the fields of the update, and saves them at once (`edit-cell.tsx`).
 * - A row shows a button for each operation the page gives a handler.
 * - A child table is this component again, with its scope filter fixed to the
 *   parent row's key, in the detail of its row (`child-tables.tsx`).
 * - An action that takes many rows and names its form is a bulk action
 *   (`bulk.tsx`).
 * - The state has its views and its place in the URL (`table-state.ts`).
 *
 * When the last load that ended read every row, a `SetTable` shows the rows,
 * with its filters, search, groups, totals and export. Otherwise the grid
 * shows them as the read returned them, and a header click on a declared sort
 * field loads again in that order. A load in progress changes neither, so a
 * reload after a write neither unmounts a set table nor flickers. The state of
 * every part stays here while the other shows.
 */

import { createEffect, createSignal, For, type JSX, Show } from "solid-js";

import { callOperation, type Outcome, rowKey, type Transport, writeMember } from "@wamn/web-runtime";

import { Button } from "../components/ui/button";
import { TextField } from "../fields";
import { createRecordLabels } from "../record-labels";
import { allowedAggregates } from "./aggregate";
import { createBulk, fillOf } from "./bulk";
import { type ChildTable, createChildAreas, expandColumn } from "./child-tables";
import { ColumnPanel } from "./column-panel";
import { type BuiltColumn, shownText, startWidth, type TableColumn, type TableSort } from "./columns";
import { createEdits } from "./edit-cell";
import type { GridDetail } from "./grid";
import { defaultGridView } from "./grid-view";
import type { QueryTableDefinition } from "./query-definition";
import { defaultQueryView } from "./query-view";
import { ScopeBar, type ScopeFilter, type ScopeMode, scopeApplies } from "./scope-bar";
import { ServerGrid } from "./server-grid";
import type { SetTable as SetTableComponent } from "./set-table";
import { defaultSetView } from "./set-view";
import { createQueryLoad, DEFAULT_CAP, fixedBy } from "./table-load";
import { createTableState } from "./table-state";
import type { TableView, ViewDeclaration } from "./view";
import { same } from "./view-parts";
import { ViewBar } from "./view-bar";

export type {
  QueryTableAction,
  QueryTableChild,
  QueryTableChoices,
  QueryTableColumn,
  QueryTableDefinition,
  QueryTableEditField,
  QueryTableFill,
  QueryTableFilter,
  QueryTableMatch,
  QueryTableSearch,
  QueryTableUpdate,
} from "./query-definition";

export interface QueryTableProps<TRow extends object, TResult = unknown> {
  readonly definition: QueryTableDefinition<TRow>;
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** What an operator calls this screen, which its outcomes name. */
  readonly label: string;
  /** Input the parent fixes, which the operator does not edit. */
  readonly fixed?: object | undefined;
  /**
   * For each operation whose record a row opens, what opening it does, keyed
   * by the operation reference. A row shows a button only for the operations
   * the page names here.
   */
  readonly onOpen?: { readonly [reference: string]: (row: TRow) => void } | undefined;
  /** For each operation whose form a row fills, what the filled values do, keyed by the operation reference. */
  readonly onFill?: { readonly [reference: string]: (initial: object) => void } | undefined;
  /** Called with every outcome of the read. */
  readonly onOutcome?: ((outcome: Outcome<TResult>) => void) | undefined;
  /**
   * The key of the table's state in the URL, such as the definition name.
   * Only a top-level table has one. A table without one stays out of the URL.
   */
  readonly urlKey?: string | undefined;
  /** The fields hidden when the table first draws. A view replaces them later. */
  readonly hiddenFields?: readonly (keyof TRow & string)[] | undefined;
  /** The fields grouped when the table first draws, in nesting order. A view replaces them later. */
  readonly groupedFields?: readonly (keyof TRow & string)[] | undefined;
  /** The time zone of a time's bucket. The default is the browser's zone. */
  readonly timeZone?: string | undefined;
  /** The ISO weekday a week bucket starts on, 1 for Monday, the default. */
  readonly weekStart?: number | undefined;
}

/** The line that says where the set features are while the set is not fully read. */
export const SET_NEEDS_FULL_SET =
  "Filters, search, grouping, totals and CSV export show when every row is read. Raise the cap to read every row.";

/** The id of the column that holds the buttons of each row. */
const ACTIONS_COLUMN = "rowActions";

/**
 * The width of the row buttons, so every button shows in full: each label at
 * 7.2 px a character (Fira Code at text-xs), 22 px of padding and border for
 * each button, 8 px between buttons, and 24 px of cell padding.
 */
const actionsWidth = (labels: readonly string[]): number =>
  Math.ceil(labels.reduce((width, label) => width + label.length * 7.2 + 22, 0) + 8 * (labels.length - 1) + 24);

type Member = { readonly [name: string]: unknown };

export function QueryTable<TRow extends object, TResult = unknown>(props: QueryTableProps<TRow, TResult>): JSX.Element {
  // The definition and the transport name one table for its whole life, and
  // the table reads its other inputs once too. A caller that changes one
  // mounts a new table, as the Receiving routes do for the order in the
  // address (wamn-erwv.6).
  /* eslint-disable solid/reactivity -- a QueryTable reads its inputs once (above). */
  const definition = props.definition;
  const transport = props.transport;
  /* eslint-enable solid/reactivity */
  const idOf = (row: TRow) => rowKey(row, definition.rowId);
  const column = (field: string) => definition.columns.find((candidate) => candidate.field === field);
  const labelOf = (field: string) => column(field)?.label ?? field;

  const scopeFields = definition.scopeFilters.filter((field) => {
    const filter = definition.filters.find((declared) => declared.field === field);
    // eslint-disable-next-line solid/reactivity -- a QueryTable reads its inputs once (above).
    return filter === undefined || !fixedBy(props.fixed, filter);
  });
  const modeOf = (field: string): ScopeMode => {
    const filter = definition.filters.find((declared) => declared.field === field);
    return {
      match: filter?.match,
      type: filter?.type,
      required: filter?.required,
      defaultLastDays: filter?.defaultLastDays,
    };
  };

  const declaration: ViewDeclaration = {
    columns: definition.columns.map((declared) => ({
      field: declared.field,
      time: declared.type === "timestamptz",
      groupable: declared.type !== "json" && declared.type !== "bytes",
      aggregates: allowedAggregates(declared.type),
    })),
    scopeFilters: scopeFields,
  };
  /* eslint-disable solid/reactivity -- a QueryTable reads its inputs once (above). */
  const defaults: TableView = {
    grid: defaultGridView(
      definition.columns.map((declared) => declared.field),
      props.hiddenFields,
    ),
    query: defaultQueryView(DEFAULT_CAP),
    set: defaultSetView(
      (props.groupedFields ?? []).map((field) => ({ field, time: column(field)?.type === "timestamptz" })),
    ),
  };
  /* eslint-enable solid/reactivity */
  // The values the scope bar holds, a band without its start among them. The query holds the ones that apply.
  const [scopeDraft, setScopeDraft] = createSignal<Readonly<Record<string, ScopeFilter>>>({});
  const state = createTableState({
    declaration,
    defaults,
    // eslint-disable-next-line solid/reactivity -- a QueryTable reads its inputs once (above).
    urlKey: props.urlKey,
    // A change of what the read is asked reads again, and so does a sort on a set that is not fully read.
    onApply: (before, after) => {
      setScopeDraft(Object.fromEntries(after.query.scope.map((scope) => [scope.field, scope])));
      const asked = (query: TableView["query"]) => [query.scope, query.find, query.cap];
      if (!same(asked(before), asked(after.query)) || (!load.complete() && !same(before.sort, after.query.sort))) {
        load.reload();
      }
    },
  });
  setScopeDraft(Object.fromEntries(state.view().query.scope.map((scope) => [scope.field, scope])));
  const query = () => state.view().query;

  const load = createQueryLoad<TRow, TResult>(definition, {
    transport,
    /* eslint-disable solid/reactivity -- a QueryTable reads its inputs once (above). */
    label: props.label,
    fixed: props.fixed,
    onOutcome: props.onOutcome,
    /* eslint-enable solid/reactivity */
    query,
  });
  load.reload();

  function changeQuery(change: Partial<TableView["query"]>) {
    state.setQuery(change);
    load.reload();
  }
  /** A sort reads again only when the set is not fully read. A fully read set sorts its rows. */
  function changeSort(sort: readonly TableSort[]) {
    state.setQuery({ sort });
    if (!load.complete()) {
      load.reload();
    }
  }
  function changeScope(filter: ScopeFilter) {
    const next = { ...scopeDraft(), [filter.field]: filter };
    setScopeDraft(next);
    changeQuery({
      scope: scopeFields.flatMap((field) => {
        const chosen = next[field];
        return chosen !== undefined && scopeApplies(chosen, modeOf(field)) ? [chosen] : [];
      }),
    });
  }

  const edits = createEdits({ definition, load, transport, idOf, sort: () => query().sort });
  const bulk = createBulk({ definition, transport, load, idOf });

  // Each child is this table again, with its scope filter fixed to the row's key.
  const childTables: readonly ChildTable[] = definition.childTables.map((child) => ({
    label: child.label,
    render: (key: string) => {
      const table = child.table();
      const filter = table.filters.find((declared) => declared.field === child.scopeFilter)!;
      return (
        <QueryTable
          definition={table}
          transport={transport}
          label={child.label}
          fixed={writeMember({}, filter.input, filter.list ? [key] : key)}
          onOpen={props.onOpen}
          onFill={props.onFill}
        />
      );
    },
  }));
  const areas = createChildAreas(() => childTables);
  createEffect(() => areas.keepOnly(new Set(load.state().rows.map(idOf))));
  const detail: GridDetail<TRow> | undefined =
    childTables.length === 0 ? undefined : { open: areas.open, render: (row) => areas.area(idOf(row), idOf(row)) };

  // A column that names a record shows the text its record read returns.
  const declaredColumns: readonly TableColumn<TRow>[] = definition.columns.map((declared) => {
    const { recordRead, displayField, ...shown } = declared;
    if (recordRead === undefined || displayField === undefined) {
      return shown;
    }
    const labels = createRecordLabels(transport, recordRead.read.route.contract.reads, async (key) => {
      const outcome = await callOperation<Member>(transport, recordRead.read, [
        writeMember({}, recordRead.keyInput, key),
      ]);
      const text = outcome.status === "completed" ? outcome.value[displayField] : null;
      return text == null ? null : String(text);
    });
    return { ...shown, cell: (value: unknown) => <>{labels(value as string | null)}</> };
  });

  // The labels of the buttons the page shows: the actions it takes.
  const shownActions = definition.actions
    // eslint-disable-next-line solid/reactivity -- a QueryTable reads its inputs once (above).
    .filter((action) => (action.opens === "record" ? props.onOpen : props.onFill)?.[action.reference])
    .map((action) => action.label);

  // The buttons of one row: each operation it opens, when the page takes it.
  const rowActions = (row: TRow) => (
    <div class="flex gap-2">
      <For each={definition.actions}>
        {(action) => {
          const opened = action.opens === "record" ? props.onOpen?.[action.reference] : undefined;
          const filled = action.opens === "form" ? props.onFill?.[action.reference] : undefined;
          return (
            <Show when={opened ?? filled}>
              <Button
                type="button"
                variant="outline"
                size="sm"
                onClick={() => (opened !== undefined ? opened(row) : filled?.(fillOf(action, row)))}
              >
                {action.label}
              </Button>
            </Show>
          );
        }}
      </For>
    </div>
  );

  // The columns as the grid renders them: the row selection, the child
  // tables' expand, the row buttons, the row edit, and the declared columns.
  // The row buttons come before any data column, so no scroll hides them.
  const columns: readonly BuiltColumn<TRow>[] = [
    ...bulk.columns(),
    ...(childTables.length === 0 ? [] : [expandColumn<TRow>(areas)]),
    ...(shownActions.length === 0
      ? []
      : [{ id: ACTIONS_COLUMN, size: actionsWidth(shownActions), header: () => "", cell: rowActions }]),
    ...edits.columns(),
    ...declaredColumns.map((declared): BuiltColumn<TRow> => ({
      id: declared.field,
      declared,
      size: startWidth(declared.type),
      cell: edits.cell(declared, (row) => declared.cell?.(row[declared.field]) ?? shownText(row[declared.field])),
    })),
  ];

  const view = state.view;
  const loaded = load.state;
  const gridClass = "h-auto min-h-0 flex-initial [&_tr:has([data-slot=table-row-conflict])]:bg-destructive/10";

  /** The set table's module loads when the table first reads a complete set. */
  const [setTable, setSetTable] = createSignal<typeof SetTableComponent>();
  createEffect(() => {
    if (load.complete() && setTable() === undefined) {
      void import("./set-table").then((module) => setSetTable(() => module.SetTable));
    }
  });
  const serverGrid = () => (
    <ServerGrid
      rows={loaded().rows}
      rowId={idOf}
      columns={columns}
      sortFields={definition.sortFields.map((sort) => sort.field)}
      sortMaxFields={definition.sortMaxFields}
      grid={view().grid}
      onGrid={state.setGrid}
      sort={query().sort}
      onSort={changeSort}
      busy={loaded().busy}
      emptyMessage={loaded().refusal}
      detail={detail}
      class={gridClass}
    />
  );

  return (
    <section data-slot="query-table" class="flex h-full min-h-0 min-w-0 flex-col gap-4">
      <Show when={scopeFields.length > 0 || query().sort.length > 0 || definition.search !== undefined}>
        <ScopeBar
          filters={scopeFields.map((field) => ({
            ...(scopeDraft()[field] ?? { field, values: [] }),
            label: labelOf(field),
            mode: modeOf(field),
          }))}
          find={definition.search === undefined ? undefined : query().find}
          sort={query().sort.map((sort) => `${labelOf(sort.field)} ${sort.direction}`)}
          onChange={changeScope}
          onFind={(find) => changeQuery({ find })}
        />
      </Show>
      <div data-slot="table-toolbar" class="flex shrink-0 flex-wrap items-end justify-between gap-4">
        <div class="flex items-end gap-2">
          <div class="w-32">
            <TextField
              label="cap"
              type="number"
              min={1}
              value={String(query().cap)}
              onChange={(value) => {
                const cap = Number(value);
                if (Number.isInteger(cap) && cap > 0) {
                  changeQuery({ cap });
                }
              }}
            />
          </div>
          <Button type="button" variant="outline" disabled={loaded().busy || edits.open()} onClick={load.reload}>
            refresh
          </Button>
          <ViewBar
            names={state.views.names()}
            chosen={state.views.chosen()}
            ignored={state.ignored}
            onPick={state.views.pick}
            onSave={state.views.save}
            onRename={state.views.rename}
            onDelete={state.views.remove}
            onReset={state.views.reset}
          />
          <ColumnPanel
            columns={view().grid.order.map((field) => ({
              id: field,
              label: labelOf(field),
              visible: !view().grid.hidden.includes(field),
            }))}
            onVisible={(id, visible) =>
              state.setGrid({
                ...view().grid,
                hidden: visible ? view().grid.hidden.filter((field) => field !== id) : [...view().grid.hidden, id],
              })
            }
            onOrder={(order) => state.setGrid({ ...view().grid, order: [...order] })}
            onShowAll={() => state.setGrid({ ...view().grid, hidden: [] })}
            onReset={() => state.setGrid({ ...view().grid, order: [...defaults.grid.order] })}
          />
        </div>
        <div data-slot="table-status" role="status" class="flex flex-col items-end gap-1 text-sm text-muted-foreground">
          <Show when={loaded().startedAt}>{(at) => <p>started {at().toLocaleTimeString()}</p>}</Show>
          <Show when={loaded().endedAt}>{(at) => <p>ended {at().toLocaleTimeString()}</p>}</Show>
          <Show when={loaded().busy}>
            <p>Loading...</p>
          </Show>
          <Show when={!loaded().busy && !loaded().fullyRead && loaded().refusal === null}>
            <p class="text-foreground">Full dataset cannot be loaded</p>
            <p>{SET_NEEDS_FULL_SET}</p>
          </Show>
        </div>
      </div>
      <bulk.Bar />
      <Show when={load.complete() && setTable()} fallback={serverGrid()}>
        {(component) => {
          const SetTable = component();
          return (
            <SetTable
              name={definition.name}
              rows={load.rows()}
              rowId={idOf}
              columns={columns}
              view={view().set}
              onView={state.setSet}
              grid={view().grid}
              onGrid={state.setGrid}
              sort={query().sort}
              onSort={changeSort}
              sortMaxFields={definition.sortMaxFields}
              detail={detail}
              emptyMessage={loaded().refusal}
              gridClass={gridClass}
              timeZone={props.timeZone}
              weekStart={props.weekStart}
            />
          );
        }}
      </Show>
      <bulk.Form />
    </section>
  );
}
