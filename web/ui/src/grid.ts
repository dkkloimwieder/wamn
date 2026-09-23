/**
 * The table features every generated table declares.
 *
 * TanStack Table 9 types a table by its features, and the type is invariant in
 * them, so one bundle gives one table type across every generated table and
 * the data grid.
 *
 * The bundle holds the features whose methods `DataGridTable` calls on every
 * render: the column order, visibility, pinning and sizing, the sort state of
 * a header, and the row pinning, selection and expanding. It holds no sorted,
 * filtered, paginated, expanded or faceted row model, because the release
 * answers sort, filter and page. A table sets `manualPagination: true`,
 * because its pages are keyset pages.
 */

import {
  columnOrderingFeature,
  columnPinningFeature,
  columnResizingFeature,
  columnSizingFeature,
  columnVisibilityFeature,
  metaHelper,
  rowExpandingFeature,
  rowPaginationFeature,
  rowPinningFeature,
  rowSelectionFeature,
  rowSortingFeature,
  tableFeatures,
} from "@tanstack/solid-table";

import type { DataGridColumnMeta } from "./blocks/data-grid/data-grid";

export const gridFeatures = tableFeatures({
  columnVisibilityFeature,
  columnOrderingFeature,
  columnPinningFeature,
  columnSizingFeature,
  // columnResizingFeature requires columnSizingFeature, declared above.
  columnResizingFeature,
  rowSortingFeature,
  rowPaginationFeature,
  rowSelectionFeature,
  rowExpandingFeature,
  rowPinningFeature,
  // biome-ignore lint/suspicious/noExplicitAny: type-only slot, shared by every row shape.
  columnMeta: metaHelper<DataGridColumnMeta<any>>(),
});

/** The feature set `gridFeatures` registers. */
export type GridFeatures = typeof gridFeatures;
