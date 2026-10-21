import "@wfe/components/styles.css";
import { render } from "@solidjs/web";
import { Shell, readScope } from "@wamn/platform-ui";
import { SECTIONS } from "./routes";

const root = document.getElementById("root");
if (root === null) throw new Error("the page has no root element");
readScope().then(
  (scope) => render(() => <Shell title="Receiving" {...scope} sections={SECTIONS} />, root),
  (error: unknown) => {
    root.textContent = String(error);
  },
);
