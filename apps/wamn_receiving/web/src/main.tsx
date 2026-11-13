import "@wfe/components/styles.css";
import { render } from "@solidjs/web";
import { Shell, readConfig } from "@wamn/platform-ui";
import { SECTIONS } from "./routes";
import { startShell } from "./sync";

startShell();
const root = document.getElementById("root");
if (root === null) throw new Error("the page has no root element");
readConfig().then(
  (target) => render(() => <Shell title="Receiving" {...target} sections={SECTIONS} />, root),
  (error: unknown) => {
    root.textContent = String(error);
  },
);
