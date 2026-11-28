import { render } from "@solidjs/web";
import "@wfe/components/styles.css";
import { Shell } from "./shell";
import { readConfig } from "./config";
import { userSections } from "./users";
import { controlSections } from "./projects";

const root = document.getElementById("app")!;
readConfig().then(
  (config) =>
    render(
      () => <Shell title="WAMN" {...config} sections={[]} controlSections={[...controlSections, ...userSections]} />,
      root,
    ),
  (error: unknown) => {
    root.textContent = String(error);
  },
);
