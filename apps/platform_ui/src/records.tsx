import { createSignal, Show, untrack, type Component } from "solid-js";
import { createTable, type NativeColumnDef } from "@wfe/components/table";
import { ValueDisplay } from "@wfe/components";
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
}) {
  const [collapsed, setCollapsed] = createSignal(true);
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
      ids: () => props.rows.map(props.rowKey),
      get: (id) => props.rows.find((row) => props.rowKey(row) === id),
    },
    columns,
  });
  const presentation = Object.fromEntries(
    columns.map((column) => [column.id, { label: String(column.header ?? column.id), renderer: column.id }]),
  );
  return (
    <TableView
      description={{
        id: props.id,
        title: props.title,
        columns: presentation,
        controls: { sorting: false, filtering: false, search: false, grouping: false, columnReordering: false },
      }}
      bindings={{
        table,
        cells,
        width: columns.some((column) => column.size !== undefined)
          ? table.getVisibleColumns().reduce((width, column) => width + column.getSize(), 0)
          : undefined,
        controlsCollapsed: collapsed(),
        onControlsCollapsedChange: setCollapsed,
      }}
    />
  );
}
