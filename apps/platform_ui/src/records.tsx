import { createMemo, createSignal, Show, untrack, type Component } from "solid-js";
import { createTable, type NativeColumnDef } from "@wfe/components/table";
import { ValueDisplay } from "@wfe/components";
import { Button, FieldError } from "@wfe/components/fields";
import { TableView, type ColumnPresentation, type CustomCellProps } from "@wfe/generator";

/** Application-owned rows and columns, presented by the shared table. */
export function Records<Row>(props: {
  id: string;
  title: string;
  rows: readonly Row[];
  rowKey: (row: Row) => string;
  columns: readonly NativeColumnDef<Row>[];
  formats?: Readonly<Record<string, ColumnPresentation["format"]>>;
  cells?: Readonly<Record<string, Component<{ row: Row }>>>;
  details?: Component<{ row: Row }>;
  create?: { label: string; run: () => void };
  refresh: () => void | Promise<void>;
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
  const rows = createMemo(() => props.rows.slice(page() * pageSize, (page() + 1) * pageSize));
  const ToolbarActions = () => (
    <Show when={props.create}>{(action) => <Button onClick={() => action().run()}>{action().label}</Button>}</Show>
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
    columns.map((column) => [column.id, { label: String(column.header ?? column.id), renderer: column.id }]),
  );
  return (
    <>
      <Show when={trouble()}>{(text) => <FieldError role="alert">{text()}</FieldError>}</Show>
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
          footerEnabled: props.rows.length > pageSize ? true : undefined,
          onApplyFilters: () => void refresh(),
          cells,
          toolbarActions: ToolbarActions,
          pagination: {
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
