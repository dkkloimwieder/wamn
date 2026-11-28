import "@wfe/components/styles.css";
import { render } from "@solidjs/web";
import { Shell, administrationSections, readAppConfig } from "@wamn/platform-ui";
import { SECTIONS } from "./routes";

const sections = [...SECTIONS, ...administrationSections];

const root = document.getElementById("root");
if (root === null) throw new Error("the page has no root element");
readAppConfig().then(
  (target) => render(() => <Shell title="Receiving" {...target} sections={sections} />, root),
  (error: unknown) => {
    root.textContent = String(error);
  },
);
