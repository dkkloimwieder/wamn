import { render } from "@solidjs/web";

import { AppShell } from "./app/app-shell";
import { createRoute } from "./routing/location";
import "./styles/tokens.css";
import "./styles/app.css";

const root = document.getElementById("root");

if (root === null) {
  throw new Error("missing client root");
}

render(() => {
  const route = createRoute();
  return <AppShell route={route()} />;
}, root);
