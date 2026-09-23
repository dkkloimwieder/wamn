/**
 * Driving one record selector the way an operator does.
 *
 * A selector is a combobox. Its list opens from the trigger beside the input,
 * and an option is chosen with a pointer. The list renders in a portal, so the
 * options are read from the whole document.
 */

import { fireEvent, screen, waitFor } from "@solidjs/testing-library";

/** The input of the selector whose label is `label`. */
export function selector(label: string): HTMLInputElement {
  return screen.getByRole("combobox", { name: label }) as HTMLInputElement;
}

/** Opens the list of one selector, and waits until it shows. */
export async function openSelector(label: string): Promise<HTMLInputElement> {
  const input = selector(label);
  const trigger = input.parentElement?.querySelector("[data-slot=combobox-trigger]");
  if (!(trigger instanceof HTMLElement)) {
    throw new Error(`the selector ${label} has no trigger`);
  }
  // An empty list does not open, and the options arrive after the first
  // render, so the trigger is pressed again until the list shows.
  await waitFor(() => {
    if (input.getAttribute("aria-expanded") !== "true") {
      fireEvent.pointerDown(trigger, { pointerType: "mouse", button: 0 });
      fireEvent.click(trigger);
      throw new Error(`the selector ${label} did not open`);
    }
  });
  return input;
}

/** Chooses the option whose text is `name` from the open list. */
export function chooseOption(name: string): void {
  const option = screen.getByRole("option", { name });
  fireEvent.pointerDown(option, { pointerType: "mouse", button: 0 });
  fireEvent.pointerUp(option, { pointerType: "mouse", button: 0 });
  fireEvent.click(option);
}

/** Opens one selector, waits for the option, and chooses it. */
export async function choose(label: string, name: string): Promise<void> {
  await openSelector(label);
  await waitFor(() => screen.getByRole("option", { name }));
  chooseOption(name);
  await waitFor(() => {
    if (selector(label).value !== name) {
      throw new Error(`the selector ${label} did not take ${name}`);
    }
  });
}
