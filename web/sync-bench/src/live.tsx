import { Show, createSignal, onCleanup } from "solid-js";
import { render } from "@solidjs/web";
import { Theme } from "@wfe/components";
import "@wfe/components/styles.css";
import { createTransport } from "@wamn/web-runtime";
import { packagingCollection, packagingFeed, type PackagingScope } from "./packaging-feed";
import type { StatusFilter } from "./order";
import { shapeA } from "./shape-a";
import { PackagingView } from "./view";
import "./style.css";

/**
 * The packaging screen on a live host: shape A over the real feed. The page
 * calls the release under `/wamn` on its own origin, and whoever serves the
 * page forwards that path to the host (`bench/follow.mjs`). `?poll=` sets the
 * follow interval in milliseconds.
 */
const params = new URLSearchParams(location.search);
const pollMs = Number(params.get("poll") ?? 10_000);
const scope: PackagingScope = {};

function App() {
  const [filter] = createSignal<StatusFilter>("all");
  const feed = packagingFeed(createTransport({ baseUrl: `${location.origin}/wamn` }));
  const c = packagingCollection(feed, scope, { pollMs });
  onCleanup(() => queueMicrotask(() => void c.cleanup()));
  const source = shapeA(c, filter);
  return (
    <Theme class="bench-page">
      <main class="bench-shapes">
        <div data-shape="a" class="bench-column">
          <h2>Packaging</h2>
          <p>
            Status: <output data-sync-status={source.status()}>{source.status()}</output>
          </p>
          <Show when={source.status() === "ready"}>
            <PackagingView label="Packaging" source={source} />
          </Show>
        </div>
      </main>
    </Theme>
  );
}

render(() => <App />, document.getElementById("root")!);
