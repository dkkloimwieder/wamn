/**
 * Exact sums and half-away-from-zero rounding for the totals of this page.
 *
 * Values are integers or decimal text. The sum runs on BigInt at the largest
 * scale among the values, so no float rounding enters it. Shape B sums with
 * `exactSum` inside its memo. Shape A sums with the float `sum` of
 * `@tanstack/db`, which has no custom aggregates, and rounds the result here.
 */

const DECIMAL = /^(-)?(\d+)(?:\.(\d+))?$/;

function parse(value: number | string): { digits: bigint; scale: number } {
  const text = typeof value === "number" ? String(value) : value;
  const match = DECIMAL.exec(text);
  if (!match) throw new Error(`not a decimal: ${text}`);
  const fraction = match[3] ?? "";
  const digits = BigInt(match[2]! + fraction);
  return { digits: match[1] ? -digits : digits, scale: fraction.length };
}

function format(digits: bigint, scale: number): string {
  const negative = digits < 0n;
  const text = (negative ? -digits : digits).toString().padStart(scale + 1, "0");
  const whole = text.slice(0, text.length - scale);
  const fraction = scale ? `.${text.slice(text.length - scale)}` : "";
  return `${negative ? "-" : ""}${whole}${fraction}`;
}

/** The exact sum of the values, as decimal text at the largest scale among them. */
export function exactSum(values: Iterable<number | string>): string {
  let total = 0n;
  let scale = 0;
  for (const value of values) {
    const next = parse(value);
    if (next.scale > scale) {
      total *= 10n ** BigInt(next.scale - scale);
      scale = next.scale;
    }
    total += next.digits * 10n ** BigInt(scale - next.scale);
  }
  return format(total, scale);
}

/** The value rounded to `places` fraction digits, ties away from zero. */
export function roundHalfAwayFromZero(value: number | string, places: number): string {
  const { digits, scale } = parse(value);
  if (scale <= places) return format(digits * 10n ** BigInt(places - scale), places);
  const divisor = 10n ** BigInt(scale - places);
  const magnitude = digits < 0n ? -digits : digits;
  const rounded = (magnitude + divisor / 2n) / divisor;
  return format(digits < 0n ? -rounded : rounded, places);
}
