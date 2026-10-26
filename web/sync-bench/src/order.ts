import type { PackagingRow } from "./generated/wamn_wms/packaging.js";

/** Which rows the screen shows: every row, or one status. */
export type StatusFilter = "all" | PackagingRow["status"];

/**
 * The display order of both shapes: the filtered rows by packaging code.
 * Both shapes call this inside a memo over their keyed row store.
 */
export function orderedIds(rows: Record<string, PackagingRow>, filter: StatusFilter): string[] {
  const shown = Object.values(rows).filter((row) => filter === "all" || row.status === filter);
  shown.sort((left, right) =>
    left.packagingCode < right.packagingCode ? -1 : left.packagingCode > right.packagingCode ? 1 : 0,
  );
  return shown.map((row) => row.id);
}
