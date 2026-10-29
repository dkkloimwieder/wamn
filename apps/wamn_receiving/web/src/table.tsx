import { createEffect, createMemo, createSignal, For, onCleanup, Show } from "solid-js";
import type { JSX } from "@solidjs/web";
import { Button, Field, Input, ValueDisplay } from "@wfe/components";
import { createTable } from "@wfe/components/table";
import { TableView, type CustomCellProps, type TableDescription } from "@wfe/generator";
import type { OperationRoute } from "@wamn/web-runtime";
import { RecordLink, type ScreenProps } from "@wamn/platform-ui";
import type { ScreenPresentation } from "./generated/wamn_receiving/components/index.js";
import { invoke, rowsOf, type RecordValue, type Rows } from "./data";
import { displayRecord, referenceLabels } from "./references";

export interface NavigationAction {
  readonly description: { readonly id: string; readonly title: string };
  readonly path: (row?: RecordValue) => string;
  readonly navigation?: boolean;
}

export function Actions(props: { screen: ScreenProps; actions?: readonly NavigationAction[]; row?: RecordValue }) {
  return (
    <div class="ui-form-actions">
      <For each={props.actions?.filter((action) => props.screen.holds(action.description.id))}>
        {(action) =>
          action.navigation ? (
            <RecordLink screen={props.screen} path={action.path(props.row)}>
              {action.description.title}
            </RecordLink>
          ) : (
            <Button onClick={() => props.screen.open(action.path(props.row))}>{action.description.title}</Button>
          )
        }
      </For>
      <For each={props.screen.actions}>
        {(action) => <Button onClick={() => props.screen.open(action.path)}>{action.label}</Button>}
      </For>
    </div>
  );
}

export function Records(props: {
  screen: ScreenProps;
  description: TableDescription & { readonly presentation: ScreenPresentation };
  route: OperationRoute;
  input?: RecordValue;
  filters?: readonly string[];
  sortable?: readonly string[];
  history?: boolean;
  actions?: readonly NavigationAction[];
  rowActions?: readonly NavigationAction[];
  recordLink?: NavigationAction;
}): JSX.Element {
  const pageSize = 25;
  const [rows, setRows] = createSignal<readonly RecordValue[]>([]);
  const [displayRows, setDisplayRows] = createSignal(new Map<RecordValue, RecordValue>());
  const [error, setError] = createSignal<string>();
  const [loading, setLoading] = createSignal(false);
  const [cursor, setCursor] = createSignal<string>();
  const [page, setPage] = createSignal(0);
  const [filters, setFilters] = createSignal<Record<string, string>>({});
  const [collapsed, setCollapsed] = createSignal(true);
  const visible = createMemo(() => rows().slice(page() * pageSize, (page() + 1) * pageSize));
  let active = true;
  let generation = 0;
  let acceptedInput: RecordValue = {};
  // The read declares the page controls it takes. A page result alone does
  // not: a read with a change feed takes no limit (wamn-fu8cj).
  const declared = props.description.presentation.page;
  const refresh = async (next = false) => {
    const current = ++generation;
    setLoading(true);
    setError(undefined);
    try {
      const filter = Object.fromEntries(
        Object.entries(filters())
          .filter(([field, value]) => value !== "" && declared?.filters.includes(field))
          .map(([field, value]) => [field, [value]]),
      );
      const sorting = table.state.sorting[0];
      const input: RecordValue = next
        ? acceptedInput
        : {
            ...props.input,
            ...(declared?.limit ? { limit: pageSize } : {}),
            ...(Object.keys(filter).length ? { filter } : {}),
            ...(sorting && declared?.sort.includes(sorting.id)
              ? { sort: { field: sorting.id, direction: sorting.desc ? "descending" : "ascending" } }
              : {}),
          };
      const result = await invoke<Rows>(props.screen.transport, props.route, {
        ...input,
        ...(next && cursor() && declared?.cursor ? { [declared.cursor]: cursor()! } : {}),
      });
      if (!active || current !== generation) return;
      acceptedInput = input;
      const received = rowsOf(result);
      const label = referenceLabels(props.screen.transport);
      const displayed = await Promise.all(
        received.map(
          async (row) => [row, await displayRecord(row, props.description.presentation.references, label)] as const,
        ),
      );
      if (!active || current !== generation) return;
      setDisplayRows(new Map([...(next ? displayRows() : []), ...displayed]));
      setRows(next ? [...rows(), ...received] : received);
      setPage(next && received.length ? page() + 1 : next ? page() : 0);
      setCursor(
        props.history
          ? received.length === pageSize
            ? String(received.at(-1)?.cursor)
            : undefined
          : (result.next_cursor ?? undefined),
      );
    } catch (error) {
      if (active && current === generation) setError(String(error));
    } finally {
      if (active && current === generation) setLoading(false);
    }
  };
  const actions = props.rowActions?.filter((action) => props.screen.holds(action.description.id)) ?? [];
  const description = {
    ...props.description,
    columns: {
      ...Object.fromEntries(
        Object.entries(props.description.columns).map(([id, column]) => [id, { ...column, renderer: id }]),
      ),
      ...(actions.length
        ? { actions: { label: "Actions", renderer: "actions", sortable: false, filterable: false } }
        : {}),
    },
    controls: { filtering: false, search: false, grouping: false },
  };
  const key = (row: RecordValue, index: number) => String(row.id ?? row.line_id ?? index);
  const table = createTable<RecordValue>({
    source: {
      ids: () => visible().map(key),
      get: (id) => visible().find((row, index) => key(row, index) === id),
    },
    columns: Object.keys(description.columns).map((id) => ({
      id,
      accessorFn: (row: RecordValue) => row[id],
      enableSorting: props.sortable?.includes(id) ?? false,
    })),
    manualSorting: true,
  });
  const cells = {
    ...Object.fromEntries(
      Object.entries(props.description.columns).map(([id, column]) => [
        id,
        (cell: CustomCellProps<RecordValue, unknown>) => {
          const value = () => displayRows().get(cell.row.original!)?.[id];
          const text = () =>
            column.format ? (
              <ValueDisplay value={value()} format={column.format} />
            ) : typeof value() === "object" && value() !== null ? (
              JSON.stringify(value())
            ) : (
              String(value() ?? "")
            );
          return props.recordLink &&
            id === props.description.presentation.displayField &&
            props.screen.holds(props.recordLink.description.id) ? (
            <RecordLink screen={props.screen} path={props.recordLink.path(cell.row.original)}>
              {text()}
            </RecordLink>
          ) : (
            text()
          );
        },
      ]),
    ),
    actions: (cell: CustomCellProps<RecordValue, unknown>) => (
      <Show when={cell.row.original}>
        {(row) => <Actions screen={{ ...props.screen, actions: undefined }} actions={actions} row={row()} />}
      </Show>
    ),
  };
  const ToolbarActions = () => <Actions screen={props.screen} actions={props.actions} />;
  const unsubscribe = props.screen.transport.onWrite?.(() => void refresh());
  onCleanup(() => {
    active = false;
    unsubscribe?.();
  });
  createEffect(
    () => undefined,
    () => void refresh(),
  );
  return (
    <div class="platform-table-screen">
      <TableView
        description={description}
        bindings={{
          table,
          cells,
          status: () => ({ loading: loading(), error: error() }),
          controlsCollapsed: collapsed(),
          onControlsCollapsedChange: setCollapsed,
          onApplyFilters: () => void refresh(),
          toolbarActions: ToolbarActions,
          pagination: {
            hasPrevious: page() > 0,
            hasNext: (page() + 1) * pageSize < rows().length || !!cursor(),
            pending: loading(),
            onPrevious: () => setPage(page() - 1),
            onNext: () => ((page() + 1) * pageSize < rows().length ? setPage(page() + 1) : void refresh(true)),
            status: `Page ${page() + 1}`,
          },
          headerEnd: (column) => (
            <Show when={column.id !== "actions" && props.filters?.includes(column.id)}>
              <Field>
                <Input
                  aria-label={props.description.columns[column.id]?.label ?? column.id}
                  value={filters()[column.id] ?? ""}
                  onValueChange={(value) => setFilters((old) => ({ ...old, [column.id]: value }))}
                />
              </Field>
            </Show>
          ),
        }}
      />
    </div>
  );
}
