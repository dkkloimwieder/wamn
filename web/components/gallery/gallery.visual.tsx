/**
 * One screenshot of each gallery section, in light mode and in dark mode
 * (wamn-7fy9.2).
 *
 * Each section is compared with its reference image under
 * `gallery/__screenshots__/`, and a changed pixel fails the test. The clock
 * stands still, so a load time or a stamped date draws the same each run. To
 * accept a change, see the README of this package.
 */

import "@wamn/ui/styles.css";

import { render } from "solid-js/web";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { page } from "vitest/browser";

import { ColorModeProvider } from "@wamn/ui";

import { Gallery } from "./gallery.js";

let dispose: (() => void) | undefined;

beforeEach(() => {
  vi.useFakeTimers({ toFake: ["Date"], now: new Date("2026-09-25T12:00:00.000Z") });
});

afterEach(() => {
  dispose?.();
  document.body.replaceChildren();
  document.documentElement.classList.remove("light", "dark");
  vi.useRealTimers();
});

/** A file name for the export names of one section. */
function slug(text: string): string {
  return text
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-|-$/g, "");
}

for (const mode of ["light", "dark"] as const) {
  it(`draws every section in ${mode} mode as its reference image`, async () => {
    document.documentElement.classList.add(mode);
    const root = document.createElement("div");
    document.body.append(root);
    dispose = render(
      () => (
        <ColorModeProvider initialColorMode={mode}>
          <Gallery />
        </ColorModeProvider>
      ),
      root,
    );
    const titles = page.getByRole("heading", { level: 2 });
    await expect.element(titles.first()).toBeVisible();
    // A section is the card around its title, and the export names it
    // shows under the title name its image, because two titles can match.
    const sections = titles.elements().map((heading) => {
      const section = heading.closest<HTMLElement>('[data-slot="card"]');
      if (section === null) {
        throw new Error(`the section ${heading.textContent ?? ""} is not a card`);
      }
      const name = section.querySelector('[data-slot="card-description"]')?.textContent ?? "";
      return { section, name: slug(name) };
    });
    const names = sections.map((section) => section.name);
    expect(new Set(names).size, "each section shows its own export names").toBe(names.length);
    for (const { section, name } of sections) {
      await expect.element(section).toMatchScreenshot(`${name}-${mode}`);
    }
  });
}
