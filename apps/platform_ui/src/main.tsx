import { render } from "@solidjs/web";
import "@wfe/components/styles.css";
import { Shell } from "./shell";
import { readScope } from "./scope";
import { userSections } from "./users";
import { controlSections } from "./projects";
import { administrationSections } from "./administration";

const root = document.getElementById("app")!;
readScope().then(
  (scope) =>
    render(
      () => (
        <Shell
          title="WAMN"
          {...scope}
          sections={administrationSections}
          controlSections={[...controlSections, ...userSections]}
        />
      ),
      root,
    ),
  (error: unknown) => {
    root.textContent = String(error);
  },
);
