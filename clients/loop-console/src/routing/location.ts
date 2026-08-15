import { createMemo, createSignal, onCleanup } from "solid-js";

import { parseHash, type Route } from "./route";

/** The current route, tracking `hashchange`. Call inside a reactive root. */
export function createRoute(): () => Route {
  const [hash, setHash] = createSignal(window.location.hash);
  const read = () => setHash(window.location.hash);
  window.addEventListener("hashchange", read);
  onCleanup(() => window.removeEventListener("hashchange", read));
  return createMemo(() => parseHash(hash()));
}
