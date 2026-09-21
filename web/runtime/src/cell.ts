/**
 * The display text of one declared value.
 *
 * The generator knows each field's contract type and writes it beside the
 * cell, so this reads the value the wire carried and states it plainly. It
 * changes no value: a decimal keeps every digit the release sent, and a 64-bit
 * integer stays a string, because a number loses precision above 2^53.
 */

import type { JsonValue } from "./wire.js";

/** The contract types that a cell can carry. */
export type CellType =
  | "boolean"
  | "int32"
  | "int64"
  | "float64"
  | "text"
  | "string"
  | "numeric"
  | "timestamptz"
  | "uuid"
  | "json"
  | "bytes"
  | "object"
  | "array";

/** What a cell shows when the value is absent or null. */
export const ABSENT_CELL = "";

/** The display text of one value that the release sent. */
export function cellText(value: JsonValue | undefined, type: CellType): string {
  if (value === undefined || value === null) {
    return ABSENT_CELL;
  }
  switch (type) {
    case "boolean":
      return value === true ? "true" : "false";
    case "timestamptz":
      return typeof value === "string" ? readableTimestamp(value) : String(value);
    case "bytes":
      return Array.isArray(value) ? `${value.length} bytes` : String(value);
    case "json":
    case "object":
    case "array":
      return typeof value === "string" ? value : JSON.stringify(value);
    default:
      return typeof value === "string" ? value : String(value);
  }
}

/**
 * One RFC 3339 timestamp, without the letters that separate its parts.
 *
 * The contract spells a timestamp in UTC to microseconds. An operator reads
 * the date and the time, so the `T` becomes a space and the trailing `Z` goes.
 * Nothing moves to a local zone, because the value states UTC.
 */
function readableTimestamp(value: string): string {
  const match = /^(\d{4}-\d{2}-\d{2})T(\d{2}:\d{2}:\d{2})(\.\d+)?Z$/.exec(value);
  if (match === null) {
    return value;
  }
  return `${match[1]} ${match[2]}`;
}
