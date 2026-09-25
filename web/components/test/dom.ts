/**
 * Direct queries for the data table tests.
 *
 * A role query walks the accessibility tree of the whole document, and a data
 * table holds many buttons, so each one costs tens of milliseconds. These read
 * the label or the text of an element instead.
 */

import { fireEvent } from "@solidjs/testing-library";

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

/** Opens the header menu of one column and picks its item named `item`, as a pointer does. */
export function pickMenu(column: string, item: string) {
  const trigger = theButton(`menu ${column}`);
  fireEvent.pointerDown(trigger, { pointerType: "mouse", button: 0 });
  fireEvent.pointerUp(trigger, { pointerType: "mouse", button: 0 });
  // A closed menu can stay in the document while it animates out, so read only this one.
  const menu = document.getElementById(trigger.getAttribute("aria-controls") ?? "");
  const found = Array.from(menu?.querySelectorAll('[role="menuitem"], [role="menuitemradio"]') ?? []).find(
    (candidate) => candidate.textContent?.trim() === item,
  );
  if (found === undefined) {
    throw new Error(`the menu of ${column} has no item ${item}`);
  }
  fireEvent.pointerDown(found, { pointerType: "mouse", button: 0 });
  fireEvent.pointerUp(found, { pointerType: "mouse", button: 0 });
  fireEvent.click(found);
  // A radio item leaves its menu open.
  if (trigger.getAttribute("aria-expanded") === "true") {
    fireEvent.keyDown(found, { key: "Escape" });
  }
}
