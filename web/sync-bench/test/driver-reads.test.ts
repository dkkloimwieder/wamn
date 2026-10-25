import { afterEach, expect, it, vi } from "vitest";
import { createTransport } from "@wamn/web-runtime";
import { packagingFeed } from "../src/packaging-feed";
import { packagingRead, trackListReads } from "../bench/reads";

afterEach(() => {
  vi.unstubAllGlobals();
  sessionStorage.clear();
});

it("counts the generated list once and recognizes a feed poll in both driver counters", async () => {
  const reads = { list: 0, changes: 0 };
  const fetch = vi.fn<typeof globalThis.fetch>(async (input) => {
    const url = new URL(String(input));
    const read = packagingRead(url);
    if (read !== undefined) reads[read] += 1;
    // The server answers the actual transport format independently of the driver.
    const value = url.searchParams.has("feed")
      ? { rows: [], next_cursor: "c2", complete: true }
      : { item: [], next_cursor: null, cursor: "c1" };
    return new Response(JSON.stringify([{ value }]));
  });
  vi.stubGlobal("fetch", fetch);
  trackListReads(packagingRead);
  const feed = packagingFeed(createTransport({ baseUrl: "http://localhost/wamn", fetch: window.fetch }));
  await feed.list({});
  await feed.changes({}, "c1");
  expect(fetch).toHaveBeenCalledTimes(2);
  expect(reads).toEqual({ list: 1, changes: 1 });
  expect(window.listReads).toEqual({ started: 1, resolved: 1 });
});
