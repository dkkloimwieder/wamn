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
 */

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

export function WindowedTable() {
  const grid = useDataGrid();
  return (
    <div
      data-slot="scroll-area-viewport"
      class="h-full w-full overflow-auto [&_tr[data-row-id]>td]:h-12 [&_tr[data-row-id]>td]:truncate"
      onMouseOver={showFullValue}
    >
      <DataGridTableVirtual
        estimateSize={ROW_HEIGHT}
        virtualizerOptions={{ enabled: grid.props.recordCount > WINDOW_FROM }}
      />
    </div>
  );
}
