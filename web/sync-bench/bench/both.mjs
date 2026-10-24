/**
 * Opens the page with both shapes at 100,000 rows and prints the row count
 * and total count that each shape shows. Run after `pnpm run build`.
 */
import { fileURLToPath } from "node:url";
import { chromium } from "playwright";
import { preview } from "vite";

const server = await preview({
  root: fileURLToPath(new URL("..", import.meta.url)),
  preview: { port: 0, host: "127.0.0.1" },
});
const browser = await chromium.launch({ channel: "chrome", headless: true });
const page = await browser.newPage({ viewport: { width: 1600, height: 900 } });
await page.goto(`${server.resolvedUrls.local[0]}?size=100000`);
await page.evaluate(() => window.bench.ready);
for (const shape of ["a", "b"]) {
  const scope = page.locator(`[data-shape="${shape}"]`);
  const rows = await scope.locator("[data-bench-rows]").getAttribute("data-bench-rows");
  const total = await scope.locator("[data-bench-total-count]").getAttribute("data-bench-total-count");
  const painted = await scope.locator(".bench-scroll tr[data-row]").count();
  console.log(`shape ${shape}: ${rows} rows, total count ${total}, ${painted} rows in the DOM`);
}
await page.screenshot({ path: process.argv[2] ?? "both.png" });
await browser.close();
await server.close();
