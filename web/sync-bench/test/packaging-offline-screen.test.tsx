import "fake-indexeddb/auto";
import { webcrypto } from "node:crypto";
import { afterEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, waitFor } from "@solidjs/testing-library";
import { Loading } from "solid-js";
import { outbox, type Persistence } from "@wfe/sync";
import { packagingOffline, packagingScope } from "../src/packaging-offline";
import { PackagingScreen } from "../src/packaging-screen";
import { fakeFeed, makeRows } from "../src/fake";
import { locationKey, type LocationRow } from "../src/location-feed";
import { packagingKey } from "../src/packaging-feed";

const close: Array<() => void | Promise<void>> = [];
afterEach(async () => {
  cleanup();
  for (const action of close.splice(0)) await action();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

function mounted(online: boolean, locations: LocationRow[] = [], saved?: Persistence) {
  vi.stubGlobal("crypto", webcrypto);
  vi.spyOn(navigator, "onLine", "get").mockReturnValue(online);
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
  const rows = makeRows(1).map((row) => ({ ...row, status: "held" as const }));
  const persistence: Persistence = saved ?? { load: async () => undefined, save: async () => {}, remove: async () => {} };
  const locationList = vi.fn(async () => ({ rows: locations, cursor: "l" }));
  const model = packagingOffline({
    packaging: fakeFeed(() => rows), scope: packagingScope("held"), persistence,
    location: { list: locationList,
      changes: async () => ({ rows: [], next_cursor: "l", complete: true }) },
    estimate: async () => ({ quota: 1_000_000, usage: 0 }),
  });
  const o = outbox({ database: `offline-screen-${crypto.randomUUID()}`, send: async () => ({ ok: true }), probe: async () => false });
  close.push(model.close, () => o.close());
  const view = render(() => <Loading><PackagingScreen collection={model.packaging.c} outbox={o}
    offline={model} filter="held" signedIn={false} updateAvailable={true} /></Loading>);
  return { model, o, locationList, ...view };
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

it.each([true, false])("records the selected local location when online=%s", async (online) => {
  const locations: LocationRow[] = [
    { id: "00000000-0000-4000-8000-000000000001", locationCode: "A", rowVersion: 1, description: null, createdAt: "2026-01-01T00:00:00Z" },
    { id: "00000000-0000-4000-8000-000000000002", locationCode: "B", rowVersion: 1, description: null, createdAt: "2026-01-01T00:00:00Z" },
  ];
  const persistence: Persistence = {
    async load(key) {
      const rows = key === locationKey ? locations : key === packagingKey(packagingScope("held"))
        ? makeRows(1).map((row) => ({ ...row, status: "held" })) : undefined;
      return rows && { rows, cursor: "saved", saved_at: "2026-01-01T00:00:00Z" };
    },
    save: async () => {}, remove: async () => {},
  };
  const view = mounted(online, locations, online ? undefined : persistence);
  if (online) {
    await view.model.packaging.c.preload();
    fireEvent.click(view.getByRole("button", { name: "Take offline" }));
  }
  await waitFor(() => expect(view.getByRole("option", { name: /^B$/ })).toBeTruthy(), { timeout: 3000 });
  expect(view.queryByLabelText("Location id")).toBeNull();
  const picker = view.getByLabelText("Location", { exact: true }) as HTMLSelectElement;
  // Playwright uses the whole label text, including any nested options.
  expect(Array.from(picker.labels ?? [], (label) => label.textContent?.trim())).toEqual(["Location"]);
  fireEvent.change(picker, { target: { value: locations[1]!.id } });
  fireEvent.input(view.getByLabelText("Packaging code"), { target: { value: "LOCAL-PICKER" } });
  fireEvent.submit(view.getByRole("form", { name: "Create packaging" }));
  await waitFor(() => expect(view.o.entries()).toHaveLength(1));
  expect(view.o.entries()[0]!.request).toMatchObject({ packagingCode: "LOCAL-PICKER", locationId: locations[1]!.id });
  expect(view.getByLabelText("Sync status").textContent).toContain("Sign in to upload");
  expect(view.locationList).toHaveBeenCalledTimes(online ? 1 : 0);
});
