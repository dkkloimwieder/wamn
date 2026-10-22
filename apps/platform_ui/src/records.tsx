import { createMemo, createSignal, onCleanup, Show, untrack, type Component } from "solid-js";
import { createTable, type TablePaginationProps, type NativeColumnDef } from "@wfe/components/table";
import { ValueDisplay } from "@wfe/components";
import { Button, FieldError } from "@wfe/components/fields";
import { TableView, type ColumnPresentation, type CustomCellProps } from "@wfe/generator";
import type { JSX } from "@solidjs/web";

export interface ServerPage<Row> {
  readonly rows: readonly Row[];
  readonly nextCursor: string | null;
  readonly total: number | null;
}

/** Keep accepted pages and cursor history in the application. */
export function createServerPage<Row>(load: (input: { limit: number; cursor?: string }) => Promise<ServerPage<Row>>) {
  const [rows, setRows] = createSignal<readonly Row[]>([]);
  const [cursors, setCursors] = createSignal<readonly (string | undefined)[]>([undefined]);
  const [next, setNext] = createSignal<string | null>(null);
  const [total, setTotal] = createSignal<number | null>(null);
  const [pending, setPending] = createSignal(false);
  const [error, setError] = createSignal<string>();
  let generation = 0;
  let active = true;
  onCleanup(() => {
    active = false;
    generation++;
  });
  const reset = () => {
    generation++;
    setCursors([undefined]);
    setNext(null);
    setTotal(null);
    setError(undefined);
    setPending(false);
  };
  const request = async (history: readonly (string | undefined)[]) => {
    if (!active) return;
    const current = ++generation;
    setPending(true);
    setError(undefined);
    try {
      const cursor = history[history.length - 1];
      const result = await load({ limit: 25, ...(cursor === undefined ? {} : { cursor }) });
      if (current !== generation) return;
      setRows(result.rows);
      setCursors(history);
      setNext(result.nextCursor);
      setTotal(result.total);
      return result;
    } catch (failure) {
      if (current === generation) setError(String(failure));
      throw failure;
    } finally {
      if (current === generation) setPending(false);
    }
  };
  const refresh = () => {
    reset();
    return request([undefined]);
  };
  const pagination = (): TablePaginationProps => ({
    hasPrevious: cursors().length > 1,
    hasNext: next() != null,
    pending: pending(),
    status: `Page ${cursors().length}`,
    ...(total() == null ? {} : { totalRows: total()! }),
    onPrevious: () => {
      if (!pending() && cursors().length > 1) void request(cursors().slice(0, -1)).catch(() => {});
    },
    onNext: () => {
      const cursor = next();
      if (!pending() && cursor != null) void request([...cursors(), cursor]).catch(() => {});
    },
  });
  return { rows, refresh, reset, pagination, error };
}

/** Application-owned rows and columns, presented by the shared table. */
export function Records<Row>(props: {
  id: string;
  title: string;
  rows: readonly Row[];
  rowKey: (row: Row) => string;
  columns: readonly (NativeColumnDef<Row> & Pick<ColumnPresentation, "sortable" | "filterable">)[];
  formats?: Readonly<Record<string, ColumnPresentation["format"]>>;
  cells?: Readonly<Record<string, Component<{ row: Row }>>>;
  details?: Component<{ row: Row }>;
  create?: { label: string; run: () => void };
  toolbarActions?: JSX.Element;
  refresh: () => unknown | Promise<unknown>;
  paging?: { pagination: () => TablePaginationProps; error: () => string | undefined };
}) {
  const [trouble, setTrouble] = createSignal<string>();
  const [refreshing, setRefreshing] = createSignal(false);
  const refresh = async () => {
    if (refreshing()) return;
    setRefreshing(true);
    setTrouble(undefined);
    try {
      await props.refresh();
    } catch (error) {
      setTrouble(String(error));
    } finally {
      setRefreshing(false);
    }
  };
  const [expanded, setExpanded] = createSignal<ReadonlySet<string>>(new Set());
  const Details = untrack(() => props.details);
  const rowDetails = Details
    ? {
        expanded: (id: string) => expanded().has(id),
        onExpandedChange: (id: string, open: boolean) =>
          setExpanded((previous) => {
            const next = new Set(previous);
            if (open) next.add(id);
            else next.delete(id);
            return next;
          }),
        render: (id: string) => (
          <Show when={props.rows.find((row) => props.rowKey(row) === id)} keyed>
            {(row) => <Details row={row} />}
          </Show>
        ),
      }
    : undefined;
  const [collapsed, setCollapsed] = createSignal(true);
  const pageSize = 25;
  const [requestedPage, setPage] = createSignal(0);
  const pageCount = createMemo(() => Math.max(1, Math.ceil(props.rows.length / pageSize)));
  const page = createMemo(() => Math.min(requestedPage(), pageCount() - 1));
  const rows = createMemo(() =>
    props.paging ? props.rows : props.rows.slice(page() * pageSize, (page() + 1) * pageSize),
  );
  const ToolbarActions = () => (
    <>
      <Show when={props.create}>{(action) => <Button onClick={() => action().run()}>{action().label}</Button>}</Show>
      {props.toolbarActions}
    </>
  );
  const columns = untrack(() => props.columns);
  const cells = untrack(() =>
    Object.fromEntries(
      columns.map((column) => {
        const Cell = props.cells?.[column.id];
        const format = props.formats?.[column.id];
        return [
          column.id,
          (cell: CustomCellProps<Row, unknown>) =>
            Cell ? (
              <Show when={cell.row.original} keyed>
                {(row) => <Cell row={row} />}
              </Show>
            ) : format ? (
              <ValueDisplay value={cell.value} format={format} />
            ) : (
              <>{String(cell.value ?? "")}</>
            ),
        ];
      }),
    ),
  );
  const table = createTable({
    source: {
      ids: () => rows().map(props.rowKey),
      get: (id) => rows().find((row) => props.rowKey(row) === id),
    },
    columns,
  });
  const presentation = Object.fromEntries(
    columns.map((column) => [
      column.id,
      {
        label: String(column.header ?? column.id),
        renderer: column.id,
        sortable: column.sortable,
        filterable: column.filterable,
      },
    ]),
  );
  return (
    <>
      <Show when={props.paging ? props.paging.error() : trouble()}>
        {(text) => <FieldError role="alert">{text()}</FieldError>}
      </Show>
      <TableView
        description={{
          id: props.id,
          title: props.title,
          columns: presentation,
          controls: { sorting: false, filtering: false, search: false, grouping: false, columnReordering: false },
        }}
        bindings={{
          table,
          rowDetails,
          footerEnabled: props.paging || props.rows.length > pageSize ? true : undefined,
          onApplyFilters: () => void refresh(),
          cells,
          toolbarActions: ToolbarActions,
          pagination: props.paging?.pagination() ?? {
            hasPrevious: page() > 0,
            hasNext: page() + 1 < pageCount(),
            onPrevious: () => setPage(page() - 1),
            onNext: () => setPage(page() + 1),
            status: `Page ${page() + 1} of ${pageCount()}`,
            totalRows: props.rows.length,
          },
          width: columns.some((column) => column.size !== undefined)
            ? table.getVisibleColumns().reduce((width, column) => width + column.getSize(), 0)
            : undefined,
          controlsCollapsed: collapsed(),
          onControlsCollapsedChange: setCollapsed,
        }}
      />
    </>
  );
}
