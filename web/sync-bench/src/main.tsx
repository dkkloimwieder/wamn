import { For, Show, createSignal, flush, onCleanup } from "solid-js";
import { render } from "@solidjs/web";
import { Button, Theme } from "@wfe/components";
import "@wfe/components/styles.css";
import { collection } from "@wfe/sync";
import { applyChanges } from "@wfe/sync/internal";
import type { Change, Feed } from "@wfe/sync";
import { fakeFeed, makeRows, randomChanges, seeded } from "./fake.js";
import type { PackagingRow, PackagingScope } from "./generated/wamn_wms/packaging.js";
import type { StatusFilter } from "./order.js";
import { shapeA } from "./shape-a.js";
import { shapeB } from "./shape-b.js";
import { PackagingView } from "./view.js";
import "./style.css";

/**
 * The measurement page. Instrumentation lives here only, never in the shapes.
 * `?shape=a|b` mounts one shape, otherwise both side by side.
 * `?size=10000|100000` picks the row count and `?seed=` the synthetic rows.
 */
type Shape = "a" | "b";
type Mounted = {
  readonly shape: Shape;
  readonly apply: (changes: Change<PackagingRow>[]) => void;
  /** Milliseconds from `list` resolving to the first painted row. */
  readonly fill: Promise<number>;
};

declare global {
  interface Window {
    /** The driver of `bench/run.mjs`. */
    bench?: {
      ready: Promise<Record<Shape, number>>;
      batch: () => Promise<Record<Shape, number>>;
      heap: () => number | null;
      /** Milliseconds for the former row_version copy, for comparison with older runs. */
      mapCost: () => number;
    };
  }
}

const params = new URLSearchParams(location.search);
const sizes = [10_000, 100_000] as const;
const size = Number(params.get("size") ?? sizes[0]);
const seed = Number(params.get("seed") ?? 1);
const only = params.get("shape");
const shapes: Shape[] = only === "a" || only === "b" ? [only] : ["a", "b"];
const scope: PackagingScope = {};

const frame = () => new Promise<number>((resolve) => requestAnimationFrame(() => resolve(performance.now())));

/** A feed that notes when its `list` answers. */
function timedFeed(rows: PackagingRow[]): { feed: Feed<PackagingRow, PackagingScope>; listed: Promise<number> } {
  const inner = fakeFeed(() => rows);
  let mark!: (time: number) => void;
  const listed = new Promise<number>((resolve) => (mark = resolve));
  return {
    listed,
    feed: {
      ...inner,
      async list(...args) {
        const page = await inner.list(...args);
        mark(performance.now());
        return page;
      },
    },
  };
}

/** Resolves with the time of the first frame after a row of `shape` is in the DOM. */
async function firstRow(shape: Shape): Promise<number> {
  const selector = `[data-shape="${shape}"] .bench-scroll tr[data-row] td`;
  while (!document.querySelector(selector)) await frame();
  flush();
  return frame();
}

function App() {
  const [filter, setFilter] = createSignal<StatusFilter>("all");
  const [batches, setBatches] = createSignal(0);
  const rows = makeRows(size, seed);
  const random = seeded(seed + 1000);
  // The page keeps its own copy of the rows to draw changes from.
  const current = new Map<string | number, PackagingRow>(rows.map((row) => [row.id, row]));
  let next = size;
  const mounted: Mounted[] = [];

  function mount(shape: Shape) {
    const { feed, listed } = timedFeed(rows);
    if (shape === "a") {
      const c = collection(feed, scope, { key: "bench", version: (row) => Number(row.rowVersion) });
      onCleanup(() => queueMicrotask(() => void c.cleanup()));
      const source = shapeA(c, filter);
      mounted.push({
        shape,
        apply: (changes) => applyChanges(c, changes),
        fill: listed.then(async (start) => (await firstRow("a")) - start),
      });
      return { source, status: source.status };
    }
    const source = shapeB(feed, scope, filter);
    mounted.push({
      shape,
      apply: (changes) => source.apply(changes),
      fill: listed.then(async (start) => (await firstRow("b")) - start),
    });
    return { source, status: source.status };
  }
  const views = shapes.map((shape) => ({ shape, ...mount(shape) }));

  async function batch(): Promise<Record<Shape, number>> {
    const changes = randomChanges([...current.values()], random, next);
    next += 20;
    for (const change of changes)
      if (change.deleted) current.delete(change.id);
      else current.set(change.id, change.data);
    const latency = {} as Record<Shape, number>;
    for (const shape of mounted) {
      const start = performance.now();
      shape.apply(changes);
      flush();
      latency[shape.shape] = (await frame()) - start;
    }
    setBatches((count) => count + 1);
    return latency;
  }

  window.bench = {
    ready: Promise.all(mounted.map(async (shape) => [shape.shape, await shape.fill] as const)).then(
      (entries) => Object.fromEntries(entries) as Record<Shape, number>,
    ),
    batch,
    mapCost: () => {
      const start = performance.now();
      rows.map((row) => ({ ...row, row_version: row.rowVersion }));
      return performance.now() - start;
    },
    heap: () => {
      (globalThis as { gc?: () => void }).gc?.();
      return (performance as { memory?: { usedJSHeapSize: number } }).memory?.usedJSHeapSize ?? null;
    },
  };

  return (
    <Theme class="bench-page">
      <header class="bench-controls">
        <h1>Sync read shapes</h1>
        <nav aria-label="Row count">
          <For each={sizes}>
            {(count) => (
              <a href={`?size=${count}`} aria-current={count === size ? "page" : undefined}>
                {count.toLocaleString("en-US")} rows
              </a>
            )}
          </For>
        </nav>
        <label>
          Status{" "}
          <select value={filter()} onChange={(event) => setFilter(event.currentTarget.value as StatusFilter)}>
            <option value="all">all</option>
            <option value="available">available</option>
            <option value="consumed">consumed</option>
            <option value="held">held</option>
          </select>
        </label>
        <Button onClick={() => void batch()}>Apply 100 changes</Button>
        <output data-bench-batches={batches()}>{batches()} batches applied</output>
      </header>
      <main class="bench-shapes">
        <For each={views}>
          {(view) => (
            <div data-shape={view.shape} class="bench-column">
              <h2>{view.shape === "a" ? "A: @wfe/sync and @tanstack/db" : "B: Solid store and memos"}</h2>
              <p>
                Status: <output data-sync-status={view.status()}>{view.status()}</output>
              </p>
              <Show when={view.status() === "ready"}>
                <PackagingView label={view.shape === "a" ? "Shape A" : "Shape B"} source={view.source} />
              </Show>
            </div>
          )}
        </For>
      </main>
    </Theme>
  );
}

render(() => <App />, document.getElementById("root")!);
