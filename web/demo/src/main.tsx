import "@wamn/ui/styles.css";

import { render } from "solid-js/web";

import { ColorModeProvider, getClientColorMode, Toaster } from "@wamn/ui";

import { App } from "./app.js";

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
      <App />
      <Toaster />
    </ColorModeProvider>
  ),
  root,
);
