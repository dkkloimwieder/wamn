/**
 * The aggregates of a table column, and the date buckets of a grouped time.
 *
 * The column type decides which aggregates a column allows. Its role decides
 * the default: a key, a reference or a revision counts, and a value takes the
 * first aggregate its type allows. A count is the number of values that are not
 * empty.
 *
 * A numeric value is decimal text. Its sum and its average use exact decimal
 * arithmetic, never a float. They show the column's scale, which is the most
 * decimal places of any loaded value. An average rounds half away from zero
 * to two places past that scale.
 */

import type { DataTableColumnRole, DataTableColumnType } from "./data-table";

/** One aggregate a column can show. */
export type DataTableAggregate = "sum" | "count" | "min" | "max" | "avg";

/** The size of one date bucket of a grouped time. */
export type DataTableBucket = "day" | "week" | "month";

/** The result of one aggregate: a number, a text value, or null for no values. */
export type AggregateResult = number | string | null;

const NUMBERS: readonly DataTableAggregate[] = ["sum", "count", "min", "max", "avg"];
const TIMES: readonly DataTableAggregate[] = ["max", "min", "count"];
const COUNT: readonly DataTableAggregate[] = ["count"];

/** The aggregates a column type allows, its default first. */
export function allowedAggregates(type: DataTableColumnType): readonly DataTableAggregate[] {
  switch (type) {
    case "int32":
    case "numeric":
    case "float64":
      return NUMBERS;
    case "timestamptz":
      return TIMES;
    default:
      return COUNT;
  }
}

/** The aggregate a column shows until the operator chooses one. */
export function defaultAggregate(
  type: DataTableColumnType,
  role: DataTableColumnRole,
): DataTableAggregate {
  return role === "value" ? allowedAggregates(type)[0]! : "count";
}

export const isEmpty = (value: unknown): boolean =>
  value === null || value === undefined || value === "";

/** A decimal as whole units of its scale: 12.5 at scale 2 is 1250. */
interface Decimal {
  readonly units: bigint;
  readonly scale: number;
}

/** The number of decimal places of one decimal text. */
export function decimalScale(text: string): number {
  const point = text.indexOf(".");
  return point === -1 ? 0 : text.length - point - 1;
}

function parseDecimal(text: string): Decimal {
  const trimmed = text.trim();
  const point = trimmed.indexOf(".");
  const digits = point === -1 ? trimmed : trimmed.slice(0, point) + trimmed.slice(point + 1);
  return { units: BigInt(digits), scale: decimalScale(trimmed) };
}

/** The units of a decimal at a scale at least its own. */
const unitsAt = (decimal: Decimal, scale: number): bigint =>
  decimal.units * 10n ** BigInt(scale - decimal.scale);

function formatDecimal(units: bigint, scale: number): string {
  const negative = units < 0n;
  const digits = (negative ? -units : units).toString().padStart(scale + 1, "0");
  const whole = scale === 0 ? digits : `${digits.slice(0, -scale)}.${digits.slice(-scale)}`;
  return negative ? `-${whole}` : whole;
}

/** The quotient of two integers, rounded half away from zero. */
function divideRounded(dividend: bigint, divisor: bigint): bigint {
  const quotient = dividend / divisor;
  const remainder = dividend % divisor;
  const twice = (remainder < 0n ? -remainder : remainder) * 2n;
  if (twice < (divisor < 0n ? -divisor : divisor)) {
    return quotient;
  }
  return quotient + ((dividend < 0n) === (divisor < 0n) ? 1n : -1n);
}

const compareNumbers = (a: number, b: number) => (a < b ? -1 : a > b ? 1 : 0);

function compareDecimals(a: string, b: string): number {
  const [x, y] = [parseDecimal(a), parseDecimal(b)];
  const scale = Math.max(x.scale, y.scale);
  const [ux, uy] = [unitsAt(x, scale), unitsAt(y, scale)];
  return ux < uy ? -1 : ux > uy ? 1 : 0;
}

/** The order of two non-empty values of one type, as an aggregate compares them. */
function compareTyped(type: DataTableColumnType, a: unknown, b: unknown): number {
  switch (type) {
    case "numeric":
      return compareDecimals(String(a), String(b));
    case "timestamptz":
      return compareNumbers(Date.parse(String(a)), Date.parse(String(b)));
    default:
      return compareNumbers(Number(a), Number(b));
  }
}

/** One aggregate over the values of one column. `scale` is the column's numeric scale. */
export function aggregateValues(
  type: DataTableColumnType,
  aggregate: DataTableAggregate,
  values: readonly unknown[],
  scale: number,
): AggregateResult {
  const present = values.filter((value) => !isEmpty(value));
  if (aggregate === "count") {
    return present.length;
  }
  if (present.length === 0) {
    return null;
  }
  if (aggregate === "min" || aggregate === "max") {
    // Each value is read once: a decimal as its units at the column's scale,
    // which is at least its own, and a time as its instant.
    const orderOf = (value: unknown): number | bigint =>
      type === "numeric"
        ? unitsAt(parseDecimal(String(value)), scale)
        : type === "timestamptz"
          ? Date.parse(String(value))
          : Number(value);
    const sign = aggregate === "min" ? -1 : 1;
    let best = present[0];
    let bestOrder = orderOf(best);
    for (const value of present) {
      const order = orderOf(value);
      if (sign * (order > bestOrder ? 1 : order < bestOrder ? -1 : 0) > 0) {
        best = value;
        bestOrder = order;
      }
    }
    return type === "numeric"
      ? formatDecimal(bestOrder as bigint, scale)
      : (best as number | string);
  }
  if (type === "numeric") {
    const sum = present.reduce<bigint>(
      (total, value) => total + unitsAt(parseDecimal(String(value)), scale),
      0n,
    );
    return aggregate === "sum"
      ? formatDecimal(sum, scale)
      : formatDecimal(divideRounded(sum * 100n, BigInt(present.length)), scale + 2);
  }
  const sum = present.reduce<number>((total, value) => total + Number(value), 0);
  return aggregate === "sum" ? sum : sum / present.length;
}

/** The order of two results of one aggregate of one column type. Null sorts last. */
export function compareAggregates(
  type: DataTableColumnType,
  aggregate: DataTableAggregate,
  a: AggregateResult,
  b: AggregateResult,
): number {
  if (a === null || b === null) {
    return (a === null ? 1 : 0) - (b === null ? 1 : 0);
  }
  return aggregate === "count" ? compareNumbers(Number(a), Number(b)) : compareTyped(type, a, b);
}

const zoneFormats = new Map<string, Intl.DateTimeFormat>();

/** The calendar date of an instant in a time zone, as year, month and day. */
function zonedDate(instant: number, timeZone: string): [number, number, number] {
  let format = zoneFormats.get(timeZone);
  if (format === undefined) {
    format = new Intl.DateTimeFormat("en-US", {
      timeZone,
      year: "numeric",
      month: "numeric",
      day: "numeric",
    });
    zoneFormats.set(timeZone, format);
  }
  const parts = Object.fromEntries(
    format.formatToParts(instant).map((part) => [part.type, part.value]),
  );
  return [Number(parts["year"]), Number(parts["month"]), Number(parts["day"])];
}

const pad = (value: number) => String(value).padStart(2, "0");

/**
 * The bucket of one time: its day, the first day of its week, or its month, in
 * the time zone. `weekStart` is the ISO weekday a week starts on, 1 for Monday.
 * An empty or unreadable time has no bucket.
 */
export function bucketOf(
  value: unknown,
  bucket: DataTableBucket,
  timeZone: string,
  weekStart: number,
): string | null {
  const instant = isEmpty(value) ? Number.NaN : Date.parse(String(value));
  if (Number.isNaN(instant)) {
    return null;
  }
  const [year, month, day] = zonedDate(instant, timeZone);
  if (bucket === "month") {
    return `${year}-${pad(month)}`;
  }
  if (bucket === "day") {
    return `${year}-${pad(month)}-${pad(day)}`;
  }
  const date = Date.UTC(year, month - 1, day);
  const weekday = new Date(date).getUTCDay() || 7;
  const start = new Date(date - ((weekday - weekStart + 7) % 7) * 86_400_000);
  return `${start.getUTCFullYear()}-${pad(start.getUTCMonth() + 1)}-${pad(start.getUTCDate())}`;
}

/** The label of a bucket's group. */
export const bucketLabel = (key: string, bucket: DataTableBucket): string =>
  bucket === "week" ? `week of ${key}` : key;
