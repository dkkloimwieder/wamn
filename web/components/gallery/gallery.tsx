/**
 * The page of the component gallery: a header with the color mode button,
 * then every section. `main.tsx` serves it, and the visual test renders it.
 */

import type { JSX } from "solid-js";

import { Button, useColorMode } from "@wamn/ui";

import { ScreenSections } from "./screens.js";
import { TableSections } from "./table.js";
import { UiSections } from "./ui.js";

export function Gallery(): JSX.Element {
  const { colorMode, toggleColorMode } = useColorMode();
  return (
    <div class="min-h-screen">
      <header class="border-b">
        <div class="mx-auto flex max-w-screen-2xl items-center justify-between gap-4 px-6 py-3">
          <h1 class="text-base font-semibold uppercase">Component gallery</h1>
          <Button variant="outline" size="sm" onClick={toggleColorMode}>
            {colorMode() === "dark" ? "light mode" : "dark mode"}
          </Button>
        </div>
      </header>
      <main class="mx-auto flex max-w-screen-2xl flex-col gap-6 px-6 py-6">
        <p class="text-sm font-semibold uppercase">web/ui components</p>
        <UiSections />
        <TableSections />
        <p class="text-sm font-semibold uppercase">Generated screens of the platform fixture</p>
        <ScreenSections />
      </main>
    </div>
  );
}
