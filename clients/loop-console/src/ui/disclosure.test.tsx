import { cleanup, fireEvent, render } from "@solidjs/testing-library";
import { createSignal, flush } from "solid-js";
import { afterEach, describe, expect, it } from "vitest";

import { failingRun } from "../reader/fixtures";
import { Disclosure } from "./disclosure";

afterEach(cleanup);

/** §2.2's `▸ raw` on the run-failure panel. */
const raw = JSON.stringify(failingRun.failure?.raw);

describe("Disclosure", () => {
  it("opens and closes itself, and creates its content only while open", () => {
    const reported: boolean[] = [];
    const { getAllByRole, getByRole } = render(() => (
      <Disclosure label="raw" onToggle={(next) => reported.push(next)}>
        <pre>{raw}</pre>
        <button type="button">copy exact</button>
      </Disclosure>
    ));

    const toggle = getByRole("button", { name: "raw" });
    const panel = () => document.getElementById(toggle.getAttribute("aria-controls") ?? "");
    expect(toggle).toHaveAttribute("aria-expanded", "false");
    expect(panel()).toBeNull();
    // closed content is gone, not hidden: its affordance is not in the tab order
    expect(getAllByRole("button")).toHaveLength(1);

    fireEvent.click(toggle);
    flush();
    expect(toggle).toHaveAttribute("aria-expanded", "true");
    expect(panel()).toHaveTextContent("retry-exhausted");
    expect(getAllByRole("button")).toHaveLength(2);

    fireEvent.click(toggle);
    flush();
    expect(toggle).toHaveAttribute("aria-expanded", "false");
    expect(panel()).toBeNull();
    expect(getAllByRole("button")).toHaveLength(1);

    // an uncontrolled disclosure still reports, so a row can act on being opened
    expect(reported).toEqual([true, false]);
  });

  it("starts open when its owner starts it open", () => {
    // §2.3's failed case rows auto-expand: they are open on the first frame
    const { getByRole } = render(() => (
      <Disclosure label="backorders-when-out-of-stock" open>
        <pre>{raw}</pre>
      </Disclosure>
    ));

    const toggle = getByRole("button", { name: "backorders-when-out-of-stock" });
    expect(toggle).toHaveAttribute("aria-expanded", "true");
    expect(document.getElementById(toggle.getAttribute("aria-controls") ?? "")).toHaveTextContent(
      "retry-exhausted",
    );
  });

  it("is a real button, named by its label and in the tab order", () => {
    const { getByRole } = render(() => (
      <Disclosure label="occurrence 1–4 · frame root">
        <p>{raw}</p>
      </Disclosure>
    ));

    // jsdom does not synthesize the platform's Enter/Space activation, so what
    // this proves is the thing that earns it: an unmodified <button>.
    const toggle = getByRole("button", { name: "occurrence 1–4 · frame root" });
    expect(toggle.tagName).toBe("BUTTON");
    expect(toggle).not.toHaveAttribute("tabindex");
    expect(toggle).toHaveAttribute("aria-controls");

    toggle.focus();
    expect(document.activeElement).toBe(toggle);
    toggle.click();
    flush();
    expect(toggle).toHaveAttribute("aria-expanded", "true");
  });

  it("moves only when its owner says so once `open` is supplied", () => {
    const [open, setOpen] = createSignal(false);
    const reported: boolean[] = [];
    const { getByRole } = render(() => (
      <Disclosure label="raw" open={open()} onToggle={(next) => reported.push(next)}>
        <pre>{raw}</pre>
      </Disclosure>
    ));

    const toggle = getByRole("button", { name: "raw" });
    fireEvent.click(toggle);
    flush();
    expect(reported).toEqual([true]);
    expect(toggle).toHaveAttribute("aria-expanded", "false");

    // step 4's expand-all: the owner moves it, and it follows
    setOpen(true);
    flush();
    expect(toggle).toHaveAttribute("aria-expanded", "true");
    expect(document.getElementById(toggle.getAttribute("aria-controls") ?? "")).toHaveTextContent(
      "retry-exhausted",
    );

    fireEvent.click(toggle);
    flush();
    expect(reported).toEqual([true, false]);
    expect(toggle).toHaveAttribute("aria-expanded", "true");
  });
});
