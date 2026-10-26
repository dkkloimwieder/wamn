import "fake-indexeddb/auto";
import { afterEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, waitFor } from "@solidjs/testing-library";
import { Loading } from "solid-js";
import { outbox, type Persistence } from "@wfe/sync";
import { packagingOffline, packagingScope } from "../src/packaging-offline";
import { PackagingScreen } from "../src/packaging-screen";
import { fakeFeed, makeRows } from "../src/fake";
import type { LocationRow } from "../src/location-feed";

const close: Array<() => void | Promise<void>> = [];
afterEach(async () => {
  cleanup();
  for (const action of close.splice(0)) await action();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

function mounted(online: boolean) {
  vi.spyOn(navigator, "onLine", "get").mockReturnValue(online);
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
  const rows = makeRows(1).map((row) => ({ ...row, status: "held" as const }));
  const persistence: Persistence = { load: async () => undefined, save: async () => {}, remove: async () => {} };
  const model = packagingOffline({
    packaging: fakeFeed(() => rows), scope: packagingScope("held"), persistence,
    location: { list: async () => ({ rows: [] as LocationRow[], cursor: "l" }),
      changes: async () => ({ rows: [], next_cursor: "l", complete: true }) },
    estimate: async () => ({ quota: 1_000_000, usage: 0 }),
  });
  const o = outbox({ database: `offline-screen-${crypto.randomUUID()}`, send: async () => ({ ok: true }), probe: async () => false });
  close.push(model.close, () => o.close());
  const view = render(() => <Loading><PackagingScreen collection={model.packaging.c} outbox={o}
    offline={model} filter="held" signedIn={false} updateAvailable={true} /></Loading>);
  return { model, ...view };
}

it("shows counts and as-of time after take-offline and preserves the status line", async () => {
  const view = mounted(true);
  await view.model.packaging.c.preload();
  fireEvent.click(view.getByRole("button", { name: "Take offline" }));
  await waitFor(() => expect(view.getByRole("status", { name: "Take offline result" }).textContent).toContain("Saved. Packaging: 1 rows"));
  const result = view.getByRole("status", { name: "Take offline result" });
  expect(result.textContent).toContain("Location: 0 rows");
  expect(result.textContent).toMatch(/Current as of: \d{4}-/);
  expect(view.getByLabelText("Sync status").textContent).toContain("update available, reload");
  expect(view.getByLabelText("Sync status").textContent).toContain("Sign in to upload");
});

it("shows needs a connection for a scope with no local collection", async () => {
  const view = mounted(false);
  await waitFor(() => expect(view.getByText("needs a connection")).toBeTruthy(), { timeout: 3000 });
  expect(view.queryByRole("form", { name: "Create packaging" })).toBeNull();
});
