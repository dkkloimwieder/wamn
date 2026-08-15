import { render } from "@solidjs/web";

const root = document.getElementById("root");

if (root === null) {
  throw new Error("missing client root");
}

render(() => null, root);
