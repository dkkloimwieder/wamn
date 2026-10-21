import { createEffect, createMemo, createSignal, For, onCleanup, Show } from "solid-js";
import type { JSX } from "@solidjs/web";
import { Field, Input, Label, ValueDisplay } from "@wfe/components";
import { createTable } from "@wfe/components/table";
import { TableView, type ColumnPresentation, type CustomCellProps, type TableDescription } from "@wfe/generator";
import type { OperationRoute } from "@wamn/web-runtime";
import { RecordLink, type ScreenProps } from "@wamn/platform-ui";
import { invoke, rowsOf, type RecordValue, type Rows } from "./data";

export interface NavigationAction {
  readonly description: { readonly id: string; readonly title: string };
  readonly path: (row?: RecordValue) => string;
}

export function Actions(props: { screen: ScreenProps; actions?: readonly NavigationAction[]; row?: RecordValue }) {
  return (
    <div class="ui-form-actions">
      <For each={props.actions?.filter((action) => props.screen.holds(action.description.id))}>
        {(action) => (
          <RecordLink screen={props.screen} path={action.path(props.row)}>
            {action.description.title}
          </RecordLink>
        )}
      </For>
      <For each={props.screen.actions}>
        {(action) => (
          <RecordLink screen={props.screen} path={action.path}>
            {action.label}
          </RecordLink>
        )}
      </For>
    </div>
  );
}

export function Records(props: {
  screen: ScreenProps;
  description: TableDescription;
  route: OperationRoute;
  input?: RecordValue;
  filters?: readonly string[];
  sortable?: readonly string[];
  history?: boolean;
  actions?: readonly NavigationAction[];
  rowActions?: readonly NavigationAction[];
}): JSX.Element {
  const pageSize = 25;
  const [rows, setRows] = createSignal<readonly RecordValue[]>([]);
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
  const paged = props.route.contract.resultClass === "page";
  const refresh = async (next = false) => {
    const current = ++generation;
    setLoading(true);
    setError(undefined);
    try {
      const filter = Object.fromEntries(
        Object.entries(filters())
          .filter(([, value]) => value !== "")
          .map(([field, value]) => [field, [value]]),
      );
      const sorting = table.state.sorting[0];
      const input: RecordValue = next
        ? acceptedInput
        : {
            ...props.input,
            ...(paged
              ? {
                  limit: pageSize,
                  ...(Object.keys(filter).length ? { filter } : {}),
                  ...(sorting
                    ? { sort: { field: sorting.id, direction: sorting.desc ? "descending" : "ascending" } }
                    : {}),
                }
              : {}),
          };
      const result = await invoke<Rows>(props.screen.transport, props.route, {
        ...input,
        ...(next && cursor() ? { [props.history ? "after_cursor" : "cursor"]: cursor()! } : {}),
      });
      if (!active || current !== generation) return;
      acceptedInput = input;
      const received = rowsOf(result);
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
  const actionColumns: Record<string, ColumnPresentation & { renderer: string }> = Object.fromEntries(
    actions.map((action) => [
      action.description.id,
      { label: action.description.title, renderer: action.description.id },
    ]),
  );
  const description = {
    ...props.description,
    columns: {
      ...Object.fromEntries(
        Object.entries(props.description.columns).map(([id, column]) => [id, { ...column, renderer: id }]),
      ),
      ...actionColumns,
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
        (cell: CustomCellProps<RecordValue, unknown>) =>
          column.format ? (
            <ValueDisplay value={cell.value} format={column.format} />
          ) : (
            <>
              {typeof cell.value === "object" && cell.value !== null
                ? JSON.stringify(cell.value)
                : String(cell.value ?? "")}
            </>
          ),
      ]),
    ),
    ...Object.fromEntries(
      actions.map((action) => [
        action.description.id,
        (cell: CustomCellProps<RecordValue, unknown>) => (
          <Show when={cell.row.original}>
            {(row) => (
              <RecordLink screen={props.screen} path={action.path(row())}>
                {action.description.title}
              </RecordLink>
            )}
          </Show>
        ),
      ]),
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
            <Show when={props.filters?.includes(column.id)}>
              <Field>
                <Label>{props.description.columns[column.id]?.label ?? column.id}</Label>
                <Input
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
