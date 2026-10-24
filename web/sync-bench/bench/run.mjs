/**
 * Measures the two read shapes in headless Chrome.
 *
 * Each cell is one shape at one size. A cell runs 5 times, each time on a new
 * page with one shape mounted. Within a repeat, the shapes alternate their
 * order, so a change in machine load falls on both. Each run records:
 * - fill: `list` resolving to the first painted row
 * - update: each of 10 batches of 100 changes, apply to the next painted frame
 * - heap after fill and after the 10 batches (performance.memory, after gc)
 * It prints the median per shape, size and metric, with the load average
 * around each cell. A run that does not finish within CELL_LIMIT_MS is
 * reported as a result, and the limit is never raised for a slow cell.
 *
 * Usage: pnpm run build, then node bench/run.mjs [--smoke] [--out file.json]
 */
import { loadavg } from "node:os";
import { writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright";
import { preview } from "vite";

const smoke = process.argv.includes("--smoke");
const out = process.argv.includes("--out") ? process.argv[process.argv.indexOf("--out") + 1] : undefined;
const sizes = smoke ? [10_000, 100_000] : [10_000, 100_000];
const repeats = smoke ? 1 : 5;
const batches = smoke ? 2 : 10;
const CELL_LIMIT_MS = 300_000;

const server = await preview({
  root: fileURLToPath(new URL("..", import.meta.url)),
  preview: { port: 0, host: "127.0.0.1" },
});
const base = server.resolvedUrls.local[0];
const browser = await chromium.launch({
  channel: "chrome",
  headless: true,
  args: ["--js-flags=--expose-gc", "--enable-precise-memory-info"],
});

const median = (values) => {
  const sorted = values.filter((value) => typeof value === "number").sort((a, b) => a - b);
  if (!sorted.length) return null;
  const middle = sorted.length >> 1;
  return sorted.length % 2 ? sorted[middle] : (sorted[middle - 1] + sorted[middle]) / 2;
};

async function run(shape, size, seed) {
  const page = await browser.newPage({ viewport: { width: 1400, height: 900 } });
  const errors = [];
  page.on("pageerror", (error) => errors.push(String(error)));
  const work = (async () => {
    await page.goto(`${base}?shape=${shape}&size=${size}&seed=${seed}`);
    await page.waitForFunction(() => window.bench !== undefined, null, { timeout: CELL_LIMIT_MS });
    const fill = await page.evaluate(async () => (await window.bench.ready)[Object.keys(await window.bench.ready)[0]]);
    const rows = await page.locator("[data-bench-rows]").getAttribute("data-bench-rows");
    const heapFill = await page.evaluate(() => window.bench.heap());
    const updates = [];
    for (let index = 0; index < batches; index++)
      updates.push(await page.evaluate(async () => Object.values(await window.bench.batch())[0]));
    const heapAfter = await page.evaluate(() => window.bench.heap());
    const count = await page.locator("[data-bench-total-count]").getAttribute("data-bench-total-count");
    return { fill, updates, heapFill, heapAfter, rows: Number(rows), totalCount: Number(count) };
  })();
  let timer;
  const limit = new Promise((_, reject) => {
    timer = setTimeout(() => reject(new Error(`did not finish in ${CELL_LIMIT_MS} ms`)), CELL_LIMIT_MS);
  });
  try {
    return { ...(await Promise.race([work, limit])), errors };
  } catch (error) {
    return { failed: String(error), errors };
  } finally {
    clearTimeout(timer);
    await page.close();
  }
}

const results = [];
for (const size of sizes) {
  const cells = { a: [], b: [] };
  const load = [loadavg()[0]];
  for (let repeat = 1; repeat <= repeats; repeat++) {
    const order = repeat % 2 ? ["a", "b"] : ["b", "a"];
    for (const shape of order) {
      const result = await run(shape, size, repeat);
      load.push(loadavg()[0]);
      cells[shape].push(result);
      console.error(
        `size ${size} repeat ${repeat} shape ${shape}: ${result.failed ?? `fill ${result.fill?.toFixed(1)} ms, rows ${result.rows}`}`,
      );
    }
  }
  for (const shape of ["a", "b"]) {
    const runs = cells[shape];
    results.push({
      shape,
      size,
      fill_ms: median(runs.map((run) => run.fill)),
      update_ms: median(runs.flatMap((run) => run.updates ?? [])),
      heap_fill_mb: median(runs.map((run) => run.heapFill && run.heapFill / 2 ** 20)),
      heap_after_mb: median(runs.map((run) => run.heapAfter && run.heapAfter / 2 ** 20)),
      finished: runs.filter((run) => !run.failed).length,
      runs: runs.length,
      load_min: Math.min(...load),
      load_max: Math.max(...load),
      failures: runs.filter((run) => run.failed).map((run) => run.failed),
      errors: runs.flatMap((run) => run.errors),
      raw: runs,
    });
  }
}

await browser.close();
await server.close();

const fixed = (value) => (value === null ? "did not finish" : value.toFixed(1));
console.log(
  "| shape | rows | fill to first paint (ms) | update per 100 changes (ms) | heap after fill (MB) | heap after 10 batches (MB) | runs finished | load average |",
);
console.log("| --- | --- | --- | --- | --- | --- | --- | --- |");
for (const row of results)
  console.log(
    `| ${row.shape.toUpperCase()} | ${row.size.toLocaleString("en-US")} | ${fixed(row.fill_ms)} | ${fixed(row.update_ms)} | ${fixed(row.heap_fill_mb)} | ${fixed(row.heap_after_mb)} | ${row.finished}/${row.runs} | ${row.load_min.toFixed(1)}–${row.load_max.toFixed(1)} |`,
  );
if (out) writeFileSync(out, JSON.stringify(results, null, 2));
