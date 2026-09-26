/**
 * The rows of one generated table, windowed when the table holds many.
 *
 * A table that holds more than `WINDOW_FROM` rows renders only the rows in
 * view of its box, so a long list stays quick. A smaller table renders every
 * row. The table still holds every row it read: paging appends, and windowing
 * only decides which rows reach the page.
 *
 * `DataGridContainer` gives the box its fixed height. The element here fills
 * that box and is the one that scrolls, and the virtual table reads it as its
 * scroll area by its slot, so the header stays sticky inside it.
 *
 * Every row is `ROW_HEIGHT` tall, so the virtual table places rows without
 * measuring them. A cell never wraps: a long value ends in an ellipsis, and
 * the pointer over the cell shows the whole value.
 *
 * An expanded row adds its expanded area below it, in a second table row. The
 * table measures that area each time it renders or changes size, and counts
 * it in the size of its row, so the rows above the view keep their height when
 * the expanded row scrolls out of it (wamn-jsct). An area keeps its last
 * height while it is out of the view.
 */

import { type JSX, createSignal, onCleanup, onMount } from "solid-js";

import { DataGridTableVirtual, useDataGrid } from "./blocks/data-grid";

/** The row count above which a table windows its rows. */
export const WINDOW_FROM = 100;

/** The height of one body row, in pixels. The `h-12` below sets it. */
export const ROW_HEIGHT = 48;

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

export function WindowedTable(props: {
  /** The rows of the table footer, such as a totals row. */
  footerContent?: JSX.Element | undefined;
}) {
  const grid = useDataGrid();
  let viewport!: HTMLDivElement;
  // The height of each row's expanded area, by row id.
  const [areas, setAreas] = createSignal<ReadonlyMap<string, number>>(new Map());
  // The same heights outside the signal: the virtualizer asks for every row's
  // size and key, and a signal read for each of 100,000 rows costs a batch of a
  // streamed load tens of milliseconds.
  let heights: ReadonlyMap<string, number> = areas();

  onMount(() => {
    // A page with no layout, as a DOM test runs, has no observer and no
    // heights to count, as the virtualizer itself does.
    if (typeof ResizeObserver === "undefined") {
      return;
    }
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
    // An expanded area renders when its row expands or scrolls into the view.
    const watch = () => viewport.querySelectorAll("tr[data-detail-for]").forEach((row) => resize.observe(row));
    const added = new MutationObserver(watch);
    added.observe(viewport, { childList: true, subtree: true });
    watch();
    onCleanup(() => {
      added.disconnect();
      resize.disconnect();
    });
  });

  type WindowedRow = { readonly id: string; getIsExpanded: () => boolean };
  // No measured area means no row has an extra height.
  const extra = (row: WindowedRow) =>
    heights.size === 0 || !row.getIsExpanded() ? 0 : (heights.get(row.id) ?? 0);

  // One options object, which the virtual table reads for every row's size.
  // Written inline in the JSX, Solid builds a new object on each read.
  const virtualizerOptions = {
    get enabled() {
      return grid.props.recordCount > WINDOW_FROM;
    },
    estimateSize: (_index: number, row: WindowedRow) => ROW_HEIGHT + extra(row),
    // The key names the size, so a new area height or an expand reads the
    // size again.
    get getItemKey() {
      areas();
      grid.table.atoms.expanded?.get();
      return (_index: number, row: WindowedRow) => {
        const height = extra(row);
        return height === 0 ? row.id : `${row.id}:${height}`;
      };
    },
  };

  return (
    <div
      ref={viewport}
      data-slot="scroll-area-viewport"
      class="h-full w-full overflow-auto [&_tr[data-row-id]>td]:h-12 [&_tr[data-row-id]>td]:truncate"
      onMouseOver={showFullValue}
    >
      <DataGridTableVirtual
        estimateSize={ROW_HEIGHT}
        virtualizerOptions={virtualizerOptions}
        footerContent={props.footerContent}
      />
    </div>
  );
}
