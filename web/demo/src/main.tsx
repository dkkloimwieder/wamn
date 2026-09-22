import { render } from "solid-js/web";

import { App } from "./app.js";

const root = document.getElementById("root");
if (root === null) {
  throw new Error("the page has no root element");
}
render(() => <App />, root);
