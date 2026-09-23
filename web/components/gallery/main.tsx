/**
 * The component gallery: every web/ui export with sample data, in light and
 * dark mode, with no stack and no network.
 *
 * It exists so that people and agents can see the components before they
 * reuse them. `npm run gallery` serves it.
 */

import "@wamn/ui/styles.css";

import { render } from "solid-js/web";

import { Button, ColorModeProvider, getClientColorMode, Toaster, useColorMode } from "@wamn/ui";

import { ScreenSections } from "./screens.js";
import { UiSections } from "./ui.js";

function Gallery() {
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
        <p class="text-sm font-semibold uppercase">Generated screens of the platform fixture</p>
        <ScreenSections />
      </main>
    </div>
  );
}

const root = document.getElementById("root");
if (root === null) {
  throw new Error("the page has no root element");
}
// The provider sets the class only when the mode changes, so the page sets
// the first one.
const mode = getClientColorMode();
document.documentElement.classList.add(mode);
render(
  () => (
    <ColorModeProvider initialColorMode={mode}>
      <Gallery />
      <Toaster />
    </ColorModeProvider>
  ),
  root,
);
