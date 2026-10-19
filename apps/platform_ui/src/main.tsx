import { render } from "@solidjs/web";
import "@wfe/components/styles.css";
import { Shell } from "./shell";
import { readScope } from "./scope";

const root = document.getElementById("app")!;
readScope().then(
  (scope) => render(() => <Shell title="WAMN" {...scope} sections={[]} />, root),
  (error: unknown) => {
    root.textContent = String(error);
  },
);
