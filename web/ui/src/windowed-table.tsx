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
 * A row is as tall as its longest value wraps, so each rendered row is
 * measured. The estimate alone would move the rows by a whole row for a
 * smaller scroll.
 */

import { measureElement } from "@tanstack/solid-virtual";

import { DataGridTableVirtual, useDataGrid } from "./blocks/data-grid";

/** The row count above which a table windows its rows. */
export const WINDOW_FROM = 100;

export function WindowedTable() {
  const grid = useDataGrid();
  return (
    <div data-slot="scroll-area-viewport" class="h-full w-full overflow-auto">
      <DataGridTableVirtual
        virtualizerOptions={{ enabled: grid.props.recordCount > WINDOW_FROM, measureElement }}
      />
    </div>
  );
}
