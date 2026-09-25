/**
 * The CSV export of a DataTable (wamn-vfvx.5).
 *
 * The text follows RFC 4180: a field that holds a comma, a quote or a line
 * break is quoted, and a quote inside it is doubled. Lines end in CRLF. The
 * text starts with a byte order mark, so a spreadsheet reads it as UTF-8.
 */

/** The text the export button shows when the set is not fully read. */
export const EXPORT_NEEDS_FULL_SET = "Export applies only to a fully read set.";

const quoteField = (field: string) => (/[",\r\n]/.test(field) ? `"${field.replaceAll('"', '""')}"` : field);

/** The CSV text of a header line and the lines under it. */
export function csvText(lines: readonly (readonly string[])[]): string {
  return `﻿${lines.map((line) => line.map(quoteField).join(",")).join("\r\n")}\r\n`;
}

/** The file name of an export: the table name, then the time in UTC. */
export const csvFileName = (name: string, at: Date) =>
  `${name}-${at.toISOString().slice(0, 19).replaceAll(":", "-")}Z.csv`;

/** Saves the text as a file, through a Blob and a temporary anchor. */
export function downloadCsv(fileName: string, text: string) {
  const url = URL.createObjectURL(new Blob([text], { type: "text/csv;charset=utf-8" }));
  const anchor = document.createElement("a");
  anchor.href = url;
  anchor.download = fileName;
  document.body.append(anchor);
  anchor.click();
  anchor.remove();
  URL.revokeObjectURL(url);
}
