/**
 * Direct queries for the data table tests.
 *
 * A role query walks the accessibility tree of the whole document, and a data
 * table holds many buttons, so each one costs tens of milliseconds. These read
 * the label or the text of an element instead.
 */

/** The button whose `aria-label`, or else whose text, is `name`, or null. */
export function button(name: string): HTMLButtonElement | null {
  return (
    Array.from(document.querySelectorAll("button")).find(
      (candidate) => (candidate.getAttribute("aria-label") ?? candidate.textContent?.trim()) === name,
    ) ?? null
  );
}

/** The button named `name`. It must exist. */
export function theButton(name: string): HTMLButtonElement {
  const found = button(name);
  if (found === null) {
    throw new Error(`no button is named ${name}`);
  }
  return found;
}

/** The rows of the table body, in the order they show. */
export const bodyRows = (): HTMLTableRowElement[] =>
  Array.from(document.querySelectorAll("tbody tr"));
