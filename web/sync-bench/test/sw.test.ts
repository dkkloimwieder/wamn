import { afterEach, expect, it, vi } from "vitest";
import { precache } from "@wfe/sync/worker";

vi.mock("@wfe/sync/worker", () => ({ precache: vi.fn() }));

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  vi.resetModules();
});

it("excludes API and identity paths from installation and fetch interception", async () => {
  const excluded = ["/wamn", "/wamn/rows?cursor=1", "/password", "/password/renew"];
  const included = ["./", "live.html", "assets/live.js", "/wamn-extra", "/passwords"];
  vi.stubGlobal("location", new URL("https://bench.test/sw.js"));
  vi.stubGlobal("__PRECACHE_MANIFEST__", [...excluded, ...included]);
  const listeners = vi.spyOn(globalThis, "addEventListener").mockImplementation(() => {});
  await import("../src/sw");
  expect(precache).toHaveBeenCalledWith(included, { apiPrefix: "/wamn", navigationFallback: "live.html" });
  const listener = listeners.mock.calls.find(([name]) => name === "fetch")![1] as EventListener;
  for (const path of [...excluded, ...included]) {
    const event = new Event("fetch");
    Object.defineProperty(event, "request", { value: { url: new URL(path, location.href).href } });
    const stop = vi.spyOn(event, "stopImmediatePropagation");
    listener(event);
    expect(stop).toHaveBeenCalledTimes(excluded.includes(path) ? 1 : 0);
  }
});
