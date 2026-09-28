/**
 * The grid under every platform table: a header row, the rows, and a footer.
 *
 * The caller owns the TanStack table. Its bundle holds at least `gridFeatures`:
 * the column order, visibility, pinning and sizing, a header edge drag that
 * sets a width, the sort state a header shows, and the row expanding of a
 * group. The grid renders what that table says and keeps no state of its own
 * beyond its measurements.
 *
 * The grid fills the box its caller sizes, and its body is the only element
 * that scrolls, so the header stays in view. A table that holds more than
 * `WINDOW_FROM` rows renders only the rows in view. Every row is `ROW_HEIGHT`
 * tall, so the rows are placed without measuring them. A cell never wraps: a
 * long value ends in an ellipsis, and the pointer over the cell shows the whole
 * value.
 *
 * A row can have a detail: an area below it, in a second table row, such as
 * its child tables. The grid measures a detail each time it renders or changes
 * size, and counts it in the size of its row, so the rows above the view keep
 * their height when the row scrolls out of it (wamn-jsct). A detail keeps its
 * last height while it is out of the view.
 *
 * The columns share the width they do not use through one fill column, whose
 * width the grid writes as the CSS variable `--data-grid-fill-size`.
 */

import {
  type Cell,
  type Column,
  columnOrderingFeature,
  columnPinningFeature,
  columnResizingFeature,
  columnSizingFeature,
  columnVisibilityFeature,
  flexRender,
  type Header,
  type Row,
  rowExpandingFeature,
  rowSelectionFeature,
  rowSortingFeature,
  type SolidTable,
  type Table,
  tableFeatures,
  type TableFeatures,
} from "@tanstack/solid-table";
import { createVirtualizer } from "@tanstack/solid-virtual";
import {
  children,
  createEffect,
  createMemo,
  createSignal,
  For,
  type JSX,
  onCleanup,
  onMount,
  Show,
  untrack,
} from "solid-js";

import { Spinner } from "../components/ui/spinner";
import { cn } from "../lib/utils";

/**
 * The features every platform table declares, whose methods the grid calls on
 * every render. A table adds the ones it runs itself, such as filtering.
 */
export const gridFeatures = tableFeatures({
  columnVisibilityFeature,
  columnOrderingFeature,
  columnPinningFeature,
  columnSizingFeature,
  // columnResizingFeature requires columnSizingFeature, declared above.
  columnResizingFeature,
  rowSortingFeature,
  rowSelectionFeature,
  rowExpandingFeature,
});

/** The feature set `gridFeatures` registers. */
export type GridFeatures = typeof gridFeatures;

/** The row count above which a table windows its rows. */
export const WINDOW_FROM = 100;

/** The height of one body row, in pixels. The `h-12` below sets it. */
export const ROW_HEIGHT = 48;

/** The rows the virtualizer places beyond the view, above and below. */
const OVERSCAN = 10;

type GridTable = SolidTable<GridFeatures, object>;
type GridRow = Row<GridFeatures, object>;
type GridColumn = Column<GridFeatures, object, unknown>;

/** The detail of a row: the rows whose detail is open, and what one shows. */
export interface GridDetail<TRow> {
  /** The ids of the rows whose detail is open. */
  readonly open: () => ReadonlySet<string>;
  /** The detail of one row, or undefined when the row has none. */
  readonly render: (row: TRow) => JSX.Element | undefined;
}

export interface GridProps<TFeatures extends TableFeatures, TRow extends object> {
  readonly table: Table<TFeatures, TRow>;
  /** True while a load is in progress. With no rows, the body says so. */
  readonly busy: boolean;
  /** What the body says when the table has no rows and no load runs. */
  readonly emptyMessage?: JSX.Element | undefined;
  /** The rows of the footer, such as a totals row. */
  readonly footer?: JSX.Element | undefined;
  readonly detail?: GridDetail<TRow> | undefined;
  /** Classes of the box, which the caller sizes. */
  readonly class?: string | undefined;
}

/**
 * The position of a pinned column. Solid applies a style object through
 * `setProperty`, so each declaration is hyphenated and has its unit.
 */
function pinningStyles(column: GridColumn): JSX.CSSProperties {
  const pinned = column.getIsPinned();
  return {
    "inset-inline-start": pinned === "start" ? `${column.getStart("start")}px` : undefined,
    "inset-inline-end": pinned === "end" ? `${column.getAfter("end")}px` : undefined,
    position: pinned ? "sticky" : undefined,
    transform: pinned ? "translateZ(0)" : undefined,
    contain: pinned ? "paint" : undefined,
    width: `${column.getSize()}px`,
    "z-index": pinned ? 30 : undefined,
    "background-clip": pinned ? "padding-box" : undefined,
  };
}

/** The width of a column's cell, from the variable the table publishes. */
const cellWidth = (id: string): JSX.CSSProperties => ({ width: `calc(var(--col-${id}-size) * 1px)` });

/** The mark of the inner edge of the pinned columns. */
const edgeOf = (column: GridColumn) => {
  const pinned = column.getIsPinned();
  return pinned === "start" && column.getIsLastColumn("start")
    ? "start"
    : pinned === "end" && column.getIsFirstColumn("end")
      ? "end"
      : undefined;
};

const PINNED_BODY_CELL =
  "data-pinned:bg-background data-pinned:isolate [&[data-pinned=start][data-last-col=start]]:shadow-[inset_-1px_0_0_0_var(--border)] [&[data-pinned=end][data-last-col=end]]:shadow-[inset_1px_0_0_0_var(--border)]";

const PINNED_HEAD_CELL = cn(
  "data-pinned:bg-muted data-outer-pinned-col:bg-clip-padding data-pinned:isolate",
  "[&[data-pinned=end]:last-child_div.cursor-col-resize:last-child]:opacity-0 [&[data-pinned=end][data-last-col=end]]:shadow-[inset_1px_0_0_0_var(--border)] [&[data-pinned=start][data-last-col=start]]:shadow-[inset_-1px_0_0_0_var(--border)]",
  "[&:not([data-pinned]):has(+[data-pinned])_div.cursor-col-resize:last-child]:opacity-0 [&[data-last-col=start]_div.cursor-col-resize:last-child]:opacity-0",
);

const ROW_BORDER =
  "[&:not(:last-child)>td]:border-b [tbody:has(+tfoot)_&:last-child>td]:border-b [*:has(>[data-slot=data-grid]+[data-slot=data-grid-pagination])_[data-slot=data-grid]_&:last-child>td]:border-b";

/** The width of the fill column, which takes the width the columns leave. */
const FILL_WIDTH = { width: "var(--data-grid-fill-size, 0px)" };

/** One cell of the fill column. */
const FillCell = () => <td aria-hidden="true" style={FILL_WIDTH} class="p-0" />;

/**
 * Shows the whole value of a cell that ends in an ellipsis, as the title of
 * the cell. It reads the width of the one cell under the pointer.
 */
function showFullValue(event: MouseEvent) {
  const cell = (event.target as Element).closest("td");
  if (cell === null) {
    return;
  }
  if (cell.scrollWidth > cell.clientWidth) {
    cell.title = cell.textContent ?? "";
  } else {
    cell.removeAttribute("title");
  }
}

function HeadCell(props: { header: Header<GridFeatures, object, unknown>; last: boolean }): JSX.Element {
  const column = () => props.header.column;
  const pinned = () => column().getIsPinned();
  const sorted = () => column().getIsSorted();
  const resizable = () => column().getCanResize();
  return (
    <th
      scope="col"
      colSpan={props.header.colSpan > 1 ? props.header.colSpan : undefined}
      aria-sort={sorted() === "asc" ? "ascending" : sorted() === "desc" ? "descending" : undefined}
      style={{
        ...(column().getCanPin() ? pinningStyles(column()) : undefined),
        width: `calc(var(--header-${props.header.id}-size) * 1px)`,
      }}
      data-pinned={pinned() || undefined}
      data-outer-pinned-col={
        pinned() === "start" && column().getIsFirstColumn("start")
          ? "start"
          : pinned() === "end" && column().getIsLastColumn("end")
            ? "end"
            : undefined
      }
      data-last-col={edgeOf(column())}
      class={cn(
        "text-foreground relative h-10 text-left align-middle font-medium rtl:text-right [&:has([role=checkbox])]:pe-0 px-3",
        resizable() && (pinned() ? "overflow-hidden" : "overflow-visible"),
        resizable() && props.last && "pe-8",
        column().getCanPin() && PINNED_HEAD_CELL,
      )}
    >
      {props.header.isPlaceholder ? null : flexRender(column().columnDef.header, props.header.getContext())}
      <Show when={resizable()}>
        <ResizeHandle header={props.header} last={props.last} />
      </Show>
    </th>
  );
}

/** The edge of a header that a drag widens, and a double click resets. */
function ResizeHandle(props: { header: Header<GridFeatures, object, unknown>; last: boolean }): JSX.Element {
  const column = () => props.header.column;
  const start = (event: MouseEvent | TouchEvent) => {
    if (event instanceof MouseEvent && event.button !== 0) {
      return;
    }
    event.preventDefault();
    event.stopPropagation();
    props.header.getResizeHandler()(event);
  };
  return (
    // A pointer affordance: a column's width has no keyboard control.
    <div
      onDblClick={() => column().resetSize()}
      onMouseDown={start}
      onTouchStart={start}
      class={cn(
        "absolute top-0 h-full cursor-col-resize user-select-none touch-none z-10 flex",
        props.last
          ? "end-0 w-5 justify-end before:hidden"
          : column().getIsPinned()
            ? // A pinned column is sticky, so the handle sits inside the cell,
              // and the pinned edge draws the separator.
              "end-0 w-5 justify-end before:hidden"
            : "-end-2 w-5 justify-center before:absolute before:inset-y-0 before:w-px before:-translate-x-px before:bg-border",
        column().getIsResizing() &&
          (props.last
            ? "before:absolute before:end-0 before:block before:inset-y-0 before:w-0.5 before:bg-primary opacity-100"
            : "before:block before:bg-primary before:w-0.5 opacity-100"),
      )}
    />
  );
}

function BodyCell(props: { cell: Cell<GridFeatures, object, unknown> }): JSX.Element {
  const column = () => props.cell.column;
  return (
    <td
      style={{
        ...(column().getCanPin() ? pinningStyles(column()) : undefined),
        ...cellWidth(column().id),
      }}
      data-pinned={column().getIsPinned() || undefined}
      data-last-col={edgeOf(column())}
      class={cn(
        "align-middle px-3 py-2",
        column().getCanResize() && "truncate",
        column().getCanPin() && PINNED_BODY_CELL,
      )}
    >
      {flexRender(column().columnDef.cell, props.cell.getContext())}
    </td>
  );
}

/** An empty cell under a pinned column, in a spacer or status row. */
function PlaceholderCell(props: { column: GridColumn }): JSX.Element {
  return (
    <td
      aria-hidden="true"
      style={{
        ...(props.column.getCanPin() ? pinningStyles(props.column) : undefined),
        ...cellWidth(props.column.id),
      }}
      data-pinned={props.column.getIsPinned() || undefined}
      data-last-col={edgeOf(props.column)}
      class={cn("p-0", props.column.getCanPin() && PINNED_BODY_CELL)}
    />
  );
}

export function Grid<TFeatures extends TableFeatures, TRow extends object>(
  props: GridProps<TFeatures, TRow>,
): JSX.Element {
  // The one widening point: the grid calls the methods of `gridFeatures`,
  // which every caller's bundle holds.
  const table = () => props.table as unknown as GridTable;
  const detail = () => props.detail as GridDetail<object> | undefined;

  const startColumns = () => table().getStartVisibleLeafColumns();
  const centerColumns = () => table().getCenterVisibleLeafColumns();
  const endColumns = () => table().getEndVisibleLeafColumns();
  const endPinned = () => (table().store.state.columnPinning.end?.length ?? 0) > 0;
  const visibleCount = () => table().getVisibleLeafColumns().length;
  const rows = () => table().getRowModel().rows as GridRow[];

  // The header groups of the three pin sections, merged into one row each.
  const headerGroups = createMemo(() => {
    const [start, center, end] = [
      table().getStartHeaderGroups(),
      table().getCenterHeaderGroups(),
      table().getEndHeaderGroups(),
    ];
    return Array.from({ length: Math.max(start.length, center.length, end.length) }, (_, index) => [
      ...(start[index]?.headers ?? []),
      ...(center[index]?.headers ?? []),
      ...(end[index]?.headers ?? []),
    ]);
  });

  // Each width is published once as a CSS variable, which the cells read.
  const sizeVariables = createMemo(() => {
    const sizes: JSX.CSSProperties = {};
    for (const header of table().getFlatHeaders()) {
      sizes[`--header-${header.id}-size`] = header.getSize();
      sizes[`--col-${header.column.id}-size`] = header.column.getSize();
    }
    return sizes;
  });
  const width = () => `calc(${table().getTotalSize()}px + var(--data-grid-fill-size, 0px))`;

  let viewport!: HTMLDivElement;
  let inner!: HTMLDivElement;

  // The fill column takes the width of the box that the columns leave.
  let boxWidth = 0;
  let fill = -1;
  const writeFill = () => {
    const next = Math.max(0, boxWidth - table().getTotalSize());
    if (next !== fill) {
      fill = next;
      inner.style.setProperty("--data-grid-fill-size", `${next}px`);
    }
  };
  // A new width or a hidden column changes the total without moving the box.
  createEffect(() => writeFill());

  // The height of each row's detail, by row id.
  const [areas, setAreas] = createSignal<ReadonlyMap<string, number>>(new Map());
  // The same heights outside the signal: the virtualizer asks for every row's
  // size and key, and a signal read for each of 100,000 rows costs a batch of a
  // streamed load tens of milliseconds.
  let heights: ReadonlyMap<string, number> = new Map();

  onMount(() => {
    const measure = () => {
      boxWidth = viewport.clientWidth;
      untrack(writeFill);
    };
    measure();
    // A page with no layout, as a DOM test runs, has no observer and no
    // heights to count, as the virtualizer itself does.
    if (typeof ResizeObserver === "undefined") {
      return;
    }
    const box = new ResizeObserver(measure);
    box.observe(viewport);
    const resize = new ResizeObserver((entries) => {
      setAreas((current) => {
        const next = new Map(current);
        for (const entry of entries) {
          const row = (entry.target as HTMLElement).dataset["detailFor"];
          const height = (entry.target as HTMLElement).getBoundingClientRect().height;
          if (row !== undefined && height > 0) {
            next.set(row, height);
          }
        }
        heights = next;
        return next;
      });
    });
    // A detail renders when its row opens it or scrolls into the view.
    const watch = () => viewport.querySelectorAll("tr[data-detail-for]").forEach((row) => resize.observe(row));
    const added = new MutationObserver(watch);
    added.observe(viewport, { childList: true, subtree: true });
    watch();
    onCleanup(() => {
      added.disconnect();
      resize.disconnect();
      box.disconnect();
    });
  });

  const windowed = () => rows().length > WINDOW_FROM;
  // No measured detail means no row has an extra height.
  const extra = (row: GridRow, open: ReadonlySet<string>) =>
    heights.size === 0 || !open.has(row.id) ? 0 : (heights.get(row.id) ?? 0);

  const virtualizer = createVirtualizer<HTMLDivElement, HTMLTableRowElement>({
    get count() {
      return rows().length;
    },
    get enabled() {
      return windowed();
    },
    getScrollElement: () => viewport,
    estimateSize: (index) => {
      const row = rows()[index];
      return ROW_HEIGHT + (row === undefined ? 0 : extra(row, untrack(() => detail()?.open()) ?? new Set()));
    },
    // The key names the size, so a new detail height or an opened detail reads
    // the size again. The virtualizer reads every size again only when its key
    // function changes.
    get getItemKey() {
      areas();
      const open = detail()?.open() ?? new Set<string>();
      return (index: number) => {
        const row = rows()[index];
        if (row === undefined) {
          return index;
        }
        const height = extra(row, open);
        return height === 0 ? row.id : `${row.id}:${height}`;
      };
    },
    overscan: OVERSCAN,
  });

  const items = () => (windowed() ? virtualizer.getVirtualItems() : []);
  const before = () => (windowed() ? (items()[0]?.start ?? 0) : 0);
  const after = () => (windowed() ? Math.max(0, virtualizer.getTotalSize() - (items().at(-1)?.end ?? 0)) : 0);

  // A row of one cell across the center columns: a spacer or a status.
  const wideRow = (content: JSX.Element, cellClass: string, style?: JSX.CSSProperties, hidden?: boolean) => (
    <tr aria-hidden={hidden || undefined}>
      <For each={startColumns()}>{(column) => <PlaceholderCell column={column} />}</For>
      <td colSpan={Math.max(centerColumns().length, 1)} class={cellClass} style={style}>
        {content}
      </td>
      <Show when={endPinned()}>
        <FillCell />
      </Show>
      <For each={endColumns()}>{(column) => <PlaceholderCell column={column} />}</For>
      <Show when={!endPinned()}>
        <FillCell />
      </Show>
    </tr>
  );
  const spacer = (height: number) => (
    <Show when={height > 0}>{wideRow(null, "p-0", { height: `${height}px`, padding: "0px" }, true)}</Show>
  );

  const bodyRow = (row: GridRow, index: () => number) => {
    // A group row expands to its rows, and has no detail of its own.
    const grouped = () => (row as { getIsGrouped?: () => boolean }).getIsGrouped?.() === true;
    const shown = createMemo(() =>
      !grouped() && detail()?.open().has(row.id) ? detail()!.render(row.original) : undefined,
    );
    return (
      <>
        <tr
          data-index={index()}
          data-row-id={row.id}
          data-depth={row.depth || undefined}
          class={cn("hover:bg-muted/40 data-[state=selected]:bg-muted/50", ROW_BORDER)}
        >
          <For each={[...row.getStartVisibleCells(), ...row.getCenterVisibleCells()]}>
            {(cell) => <BodyCell cell={cell} />}
          </For>
          <Show when={endPinned()}>
            <FillCell />
          </Show>
          <For each={row.getEndVisibleCells()}>{(cell) => <BodyCell cell={cell} />}</For>
          <Show when={!endPinned()}>
            <FillCell />
          </Show>
        </tr>
        <Show when={shown()}>
          {(content) => (
            <tr data-detail-for={row.id} class={ROW_BORDER}>
              <td colSpan={row.getVisibleCells().length + 1}>{content()}</td>
            </tr>
          )}
        </Show>
      </>
    );
  };

  // Resolve once: the footer is JSX handed in as a prop, and testing it for
  // presence must not build it a second time.
  const footer = children(() => props.footer);

  return (
    <div data-slot="data-grid" class={cn("h-[32rem] w-full overflow-auto", props.class)}>
      <div
        ref={viewport}
        data-slot="scroll-area-viewport"
        class="h-full w-full overflow-auto [&_tr[data-row-id]>td]:h-12 [&_tr[data-row-id]>td]:truncate"
        onMouseOver={showFullValue}
      >
        <div ref={inner} class="relative min-w-full align-top" style={{ width: width() }}>
          <table
            data-slot="data-grid-table"
            class="text-foreground caption-bottom text-left align-middle text-sm font-normal rtl:text-right min-w-0 table-fixed border-separate border-spacing-0"
            style={{ ...sizeVariables(), width: width() }}
          >
            <colgroup>
              <For each={[...startColumns(), ...centerColumns()]}>
                {(column) => <col style={cellWidth(column.id)} />}
              </For>
              <Show when={endPinned()}>
                <col style={FILL_WIDTH} />
              </Show>
              <For each={endColumns()}>{(column) => <col style={cellWidth(column.id)} />}</For>
              <Show when={!endPinned()}>
                <col style={FILL_WIDTH} />
              </Show>
            </colgroup>
            <thead class="sticky top-0 z-40 bg-background/90 backdrop-blur-xs">
              <For each={headerGroups()}>
                {(headers) => (
                  <tr class="[&>th]:border-b bg-transparent">
                    <For each={headers.filter((header) => header.column.getIsPinned() !== "end")}>
                      {(header) => <HeadCell header={header} last={header.column.getIndex() === visibleCount() - 1} />}
                    </For>
                    <Show when={endPinned()}>
                      <th aria-hidden="true" style={FILL_WIDTH} class="p-0" />
                    </Show>
                    <For each={headers.filter((header) => header.column.getIsPinned() === "end")}>
                      {(header) => <HeadCell header={header} last={header.column.getIndex() === visibleCount() - 1} />}
                    </For>
                    <Show when={!endPinned()}>
                      <th aria-hidden="true" style={FILL_WIDTH} class="p-0" />
                    </Show>
                  </tr>
                )}
              </For>
            </thead>
            <tbody data-slot="data-grid-table-body">
              <Show
                when={rows().length > 0}
                fallback={
                  // A first load does not show the empty state as if the read returned nothing.
                  <Show
                    when={props.busy}
                    fallback={
                      <tr>
                        <td
                          colSpan={Math.max(visibleCount() + 1, 1)}
                          class="text-muted-foreground py-6 text-center text-sm"
                        >
                          {props.emptyMessage || "No data available"}
                        </td>
                      </tr>
                    }
                  >
                    {wideRow(
                      <div class="flex items-center justify-center gap-2">
                        <Spinner class="size-4 opacity-60" />
                        Loading...
                      </div>,
                      "text-muted-foreground py-4 text-center text-sm",
                    )}
                  </Show>
                }
              >
                <Show when={windowed()} fallback={<For each={rows()}>{(row, index) => bodyRow(row, index)}</For>}>
                  {spacer(before())}
                  <For each={items()}>
                    {(item) => <Show when={rows()[item.index]}>{(row) => bodyRow(row(), () => item.index)}</Show>}
                  </For>
                  {spacer(after())}
                </Show>
              </Show>
            </tbody>
            <Show when={footer()}>
              <tfoot data-slot="data-grid-table-foot">{footer()}</tfoot>
            </Show>
          </table>
        </div>
      </div>
    </div>
  );
}

/** One row of the footer. */
export function GridFootRow(props: { children: JSX.Element }): JSX.Element {
  return (
    <tr data-slot="data-grid-table-foot-row" class="[&:not(:last-child)>td]:border-b">
      {props.children}
      <FillCell />
    </tr>
  );
}

/** One cell of a footer row. */
export function GridFootCell(props: { children?: JSX.Element }): JSX.Element {
  return <td class="text-secondary-foreground/80 align-middle font-medium px-3 py-2">{props.children}</td>;
}
