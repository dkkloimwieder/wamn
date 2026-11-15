import "fake-indexeddb/auto";
import { webcrypto } from "node:crypto";
import { afterEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, waitFor } from "@solidjs/testing-library";
import { Loading, createSignal, flush } from "solid-js";
import { outbox, type Persistence } from "@wfe/sync";
import { packagingOffline, packagingFilter } from "../src/packaging-offline.js";
import { PackagingScreen } from "../src/packaging-screen.js";
import { fakeFeed, makeRows } from "../src/fake.js";
import { locationKey, type LocationRow } from "../src/generated/wamn_wms/location.js";
import { packagingKey } from "../src/generated/wamn_wms/packaging.js";

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
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe() {}
      unobserve() {}
      disconnect() {}
    },
  );
  const rows = makeRows(1).map((row) => ({ ...row, status: "held" as const }));
  const persistence: Persistence = saved ?? {
    load: async () => undefined,
    save: async () => {},
    remove: async () => {},
  };
  const locationList = vi.fn(async () => ({ rows: locations, cursor: "l" }));
  const model = packagingOffline({
    packaging: fakeFeed(() => rows),
    filter: packagingFilter("held"),
    persistence,
    location: { list: locationList, changes: async () => ({ rows: [], next_cursor: "l", complete: true }) },
    estimate: async () => ({ quota: 1_000_000, usage: 0 }),
  });
  const o = outbox({
    database: `offline-screen-${crypto.randomUUID()}`,
    send: async () => ({ ok: true }),
    probe: async () => false,
  });
  close.push(model.close, () => o.close());
  const view = render(() => (
    <Loading>
      <PackagingScreen
        collection={model.packaging.c}
        outbox={o}
        offline={model}
        filter="held"
        signedIn={false}
        updateAvailable={true}
      />
    </Loading>
  ));
  return { model, o, locationList, ...view };
}

it("announces a completed offline save and preserves the sync controls", async () => {
  const view = mounted(true);
  const save = vi.spyOn(view.model, "takeOffline");
  await view.model.packaging.c.preload();
  fireEvent.click(view.getByRole("button", { name: "Take offline" }));
  await waitFor(() => expect(save).toHaveBeenCalledTimes(1));
  const saved = await save.mock.results[0]!.value;
  expect(saved.packaging.count).toBe(view.model.packaging.c.size);
  expect(saved.location.count).toBe(view.model.location!.c.size);
  expect(Number.isNaN(Date.parse(saved.packaging.asOf))).toBe(false);
  const result = view.getByRole("status", { name: "Take offline result" });
  await waitFor(() => expect(result.textContent).toContain(saved.packaging.asOf));
  expect(result.textContent).toContain(saved.location.asOf);
  expect(view.getByLabelText("Sync status")).toBeTruthy();
});

it("refuses a create for an offline filter with no local collection", async () => {
  const view = mounted(false);
  await view.model.restored;
  expect(view.model.packaging.local()).toBe(false);
  expect(view.model.packaging.c.size).toBe(0);
  expect(view.queryByRole("form", { name: "Create packaging" })).toBeNull();
});

it.each([false, true])(
  "renders an offline reopen and update notice with a stored expected row=%s",
  async (restoreExpected) => {
    vi.stubGlobal("crypto", webcrypto);
    vi.spyOn(navigator, "onLine", "get").mockReturnValue(false);
    vi.stubGlobal(
      "ResizeObserver",
      class {
        observe() {}
        unobserve() {}
        disconnect() {}
      },
    );
    vi.spyOn(HTMLElement.prototype, "offsetHeight", "get").mockImplementation(function (this: HTMLElement) {
      return this.tagName === "THEAD" ? 32 : 480;
    });
    vi.spyOn(HTMLElement.prototype, "offsetWidth", "get").mockReturnValue(900);
    vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockReturnValue({
      x: 0,
      y: 0,
      top: 0,
      left: 0,
      bottom: 480,
      right: 900,
      width: 900,
      height: 480,
      toJSON() {},
    });
    const row = { ...makeRows(1)[0]!, status: "held" as const };
    const location: LocationRow = {
      id: row.locationId,
      locationCode: "SAVED-LOCATION",
      rowVersion: "1",
      description: null,
      createdAt: "2026-01-01T00:00:00Z",
    };
    const expected = {
      id: crypto.randomUUID(),
      packagingCode: "RESTORED",
      locationId: row.locationId,
      type: "bin",
      status: "held",
    };
    const database = `offline-reopen-${crypto.randomUUID()}`;
    const send = vi.fn(async () => ({ ok: true as const }));
    const previous = outbox({ database, send, probe: async () => false });
    await previous.enqueue(
      {
        operation: "packaging/create",
        request: expected,
        request_hash: "restored-create",
        expect: { model: "packaging", id: expected.id },
        append_only: true,
      },
      restoreExpected ? { ...expected, $tentative: true } : undefined,
    );
    await previous.replay();
    previous.close();
    const o = outbox({ database, send, probe: async () => false });
    await waitFor(() => expect(o.entries()).toHaveLength(1));
    let release!: () => void;
    const loaded = new Promise<void>((resolve) => {
      release = resolve;
    });
    const persistence: Persistence = {
      async load(key) {
        await loaded;
        return {
          rows: key === locationKey({}) ? [location] : [row],
          cursor: "saved",
          saved_at: "2026-01-01T00:00:00Z",
        };
      },
      save: async () => {},
      remove: async () => {},
    };
    const packagingList = vi.fn(async () => ({ rows: [row], cursor: "p" }));
    const locationList = vi.fn(async () => ({ rows: [location], cursor: "l" }));
    const model = packagingOffline({
      packaging: { list: packagingList, changes: async () => ({ rows: [], next_cursor: "p", complete: true }) },
      location: { list: locationList, changes: async () => ({ rows: [], next_cursor: "l", complete: true }) },
      filter: packagingFilter("held"),
      persistence,
    });
    close.push(model.close, () => o.close());
    let update!: () => void;
    const view = render(() => {
      const [available, setAvailable] = createSignal(false);
      update = () => setAvailable(true);
      return (
        <Loading>
          <PackagingScreen
            collection={model.packaging.c}
            outbox={o}
            offline={model}
            filter="held"
            signedIn={false}
            updateAvailable={available()}
          />
        </Loading>
      );
    });
    flush();
    await new Promise((resolve) => setTimeout(resolve, 0));
    release();
    await model.restored;
    await model.packaging.c.preload();
    await model.location!.c.preload();
    update();
    flush();
    expect(model.packaging.c.get(row.id)).toBeDefined();
    expect(model.location!.c.get(location.id)).toBeDefined();
    expect(o.entries()[0]?.state).toBe("pending");
    expect(send).not.toHaveBeenCalled();
    expect(packagingList).not.toHaveBeenCalled();
    expect(locationList).not.toHaveBeenCalled();
    const visible = () => ({
      row: view.container.querySelector(`tr[data-row="${row.id}"]`) !== null,
      picker: view.queryByRole("option", { name: "SAVED-LOCATION" }) !== null,
      update: view.getByLabelText("Sync status").textContent!.includes("update available, reload"),
    });
    try {
      await waitFor(() => expect(visible()).toEqual({ row: true, picker: true, update: true }));
      // The restore starts after the saved rows land.
      if (restoreExpected)
        await waitFor(() =>
          expect(view.container.querySelector(`tr[data-row="${expected.id}"]`)?.textContent).toContain(
            "RESTORED (tentative)",
          ),
        );
    } finally {
      // Ending the restored action must release the same reads without another load.
      await o.discard(o.entries()[0]!.seq);
      await waitFor(() => expect(visible()).toEqual({ row: true, picker: true, update: true }));
    }
  },
);

it.each([true, false])("records the selected local location when online=%s", async (online) => {
  const locations: LocationRow[] = [
    {
      id: "00000000-0000-4000-8000-000000000001",
      locationCode: "A",
      rowVersion: "1",
      description: null,
      createdAt: "2026-01-01T00:00:00Z",
    },
    {
      id: "00000000-0000-4000-8000-000000000002",
      locationCode: "B",
      rowVersion: "1",
      description: null,
      createdAt: "2026-01-01T00:00:00Z",
    },
  ];
  const persistence: Persistence = {
    async load(key) {
      const rows =
        key === locationKey({})
          ? locations
          : key === packagingKey(packagingFilter("held"))
            ? makeRows(1).map((row) => ({ ...row, status: "held" }))
            : undefined;
      return rows && { rows, cursor: "saved", saved_at: "2026-01-01T00:00:00Z" };
    },
    save: async () => {},
    remove: async () => {},
  };
  const view = mounted(online, locations, online ? undefined : persistence);
  if (online) {
    await view.model.packaging.c.preload();
    fireEvent.click(view.getByRole("button", { name: "Take offline" }));
  }
  await waitFor(() => expect(view.getByRole("option", { name: /^B$/ })).toBeTruthy(), { timeout: 3000 });
  expect(view.queryByLabelText("Location id")).toBeNull();
  const picker = view.getByLabelText("Location", { exact: true }) as HTMLSelectElement;
  expect(view.getByRole("combobox", { name: "Location" })).toBe(picker);
  fireEvent.change(picker, { target: { value: locations[1]!.id } });
  fireEvent.input(view.getByLabelText("Packaging code"), { target: { value: "LOCAL-PICKER" } });
  fireEvent.submit(view.getByRole("form", { name: "Create packaging" }));
  await waitFor(() => expect(view.o.entries()).toHaveLength(1));
  {
    const observed = view.o.entries()[0]!.request;
    expect(observed).toHaveProperty(["packagingCode"], "LOCAL-PICKER");
    expect(observed).toHaveProperty(["locationId"], locations[1]!.id);
  }
  expect(view.o.entries()[0]!.state).toBe("pending");
  expect(view.locationList).toHaveBeenCalledTimes(online ? 1 : 0);
});
