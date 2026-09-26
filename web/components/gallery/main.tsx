/**
 * The component gallery: every web/ui export with sample data, in light and
 * dark mode, with no stack and no network.
 *
 * It exists so that people and agents can see the components before they
 * reuse them. `pnpm run gallery` serves it.
 */

import "@wamn/ui/styles.css";

import { render } from "solid-js/web";

import { ColorModeProvider, getClientColorMode, Toaster } from "@wamn/ui";

import { Gallery } from "./gallery.js";
import { APP_TABLE_ONLY, AppTable } from "./table.js";

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
      {/* The app-shaped table alone, so a measurement reads that shape only. */}
      {new URLSearchParams(location.search).has(APP_TABLE_ONLY) ? <AppTable /> : <Gallery />}
      <Toaster />
    </ColorModeProvider>
  ),
  root,
);
