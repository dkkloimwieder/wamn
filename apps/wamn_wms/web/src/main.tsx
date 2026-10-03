import "@wamn/ui/styles.css";

import { render } from "solid-js/web";

import { Shell, readScope } from "@wamn/shell";
import { ColorModeProvider, getClientColorMode } from "@wamn/ui";

import { SECTIONS } from "./routes.js";

const root = document.getElementById("root");
if (root === null) {
  throw new Error("the page has no root element");
}
// The provider sets the class only when the mode changes, so the page sets
// the first one.
const mode = getClientColorMode();
document.documentElement.classList.add(mode);
// The org and the project come from config.json, so one build serves every deployment.
readScope().then(
  (scope) =>
    render(
      () => (
        <ColorModeProvider initialColorMode={mode}>
          <Shell title="WMS" org={scope.org} project={scope.project} sections={SECTIONS} />
        </ColorModeProvider>
      ),
      root,
    ),
  (error: unknown) => {
    root.textContent = String(error);
  },
);
