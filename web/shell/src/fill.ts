/**
 * The values a row fills into a form, carried in the query of the form
 * address. One query value holds one member, named by its path, such as
 * `value.palletId`, so a reload of the form address keeps the values.
 */

/** The path of a form with the values a row fills, such as `inventory/move?value.palletId=<id>`. */
export function fillPath(path: string, values: object): string {
  const query = new URLSearchParams();
  const walk = (value: unknown, name: string) => {
    if (typeof value === "object" && value !== null) {
      for (const [member, inner] of Object.entries(value)) {
        walk(inner, name === "" ? member : `${name}.${member}`);
      }
    } else if (value !== undefined) {
      query.set(name, String(value));
    }
  };
  walk(values, "");
  const text = query.toString();
  return text === "" ? path : `${path}?${text}`;
}

/** The values a form starts with, from the query values that `fillPath` wrote. */
export function filledValues<T>(search: Readonly<Record<string, string | undefined>>): T {
  const values: Record<string, unknown> = {};
  for (const [name, text] of Object.entries(search)) {
    const members = name.split(".");
    const last = members.pop();
    if (text === undefined || last === undefined) {
      continue;
    }
    let inner = values;
    for (const member of members) {
      inner = (inner[member] ??= {}) as Record<string, unknown>;
    }
    inner[last] = text;
  }
  return values as T;
}
