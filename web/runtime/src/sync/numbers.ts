/** Show a pending number without text that can parse as a number. */
export function pendingNumber(id: string): string {
  return `pending·${id.replaceAll("-", "").slice(-4).toLowerCase()}`;
}

/** Show the host-assigned number once the feed brings it. */
export function shownNumber<Row extends { id: string }, Field extends keyof Row>(
  row: Row,
  field: Field,
): NonNullable<Row[Field]> | string {
  return row[field] ?? pendingNumber(row.id);
}

/** Whether the feed supplied the number, including zero. */
export function hasNumber<Row, Field extends keyof Row>(row: Row, field: Field): boolean {
  return row[field] !== undefined && row[field] !== null;
}
