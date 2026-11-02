// @vitest-environment node
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, expect, it, vi } from "vitest";
import { preparedPreview } from "../bench/preview.js";

afterEach(() => vi.unstubAllEnvs());

it("requires a prepared screen instead of using an implicit output directory", () => {
  vi.stubEnv("WAMN_SYNC_BENCH_DIST", "");
  expect(() => preparedPreview()).toThrow("WAMN_SYNC_BENCH_DIST is required");
});

it("serves the explicit screen output under the application audience", async () => {
  const directory = await mkdtemp(join(tmpdir(), "wamn-sync-bench-preview-"));
  let server: Awaited<ReturnType<typeof preparedPreview>> | undefined;
  try {
    await writeFile(join(directory, "live.html"), "<!doctype html><title>Prepared screen</title>");
    vi.stubEnv("WAMN_SYNC_BENCH_DIST", directory);
    server = await preparedPreview();
    const response = await fetch(
      `${server.resolvedUrls!.local[0]}urn:wamn:project-env:acme:wms:dev:localapp/live.html`,
    );
    expect(response.status).toBe(200);
    expect(await response.text()).toContain("<title>Prepared screen</title>");
  } finally {
    await server?.close();
    await rm(directory, { recursive: true, force: true });
  }
});
