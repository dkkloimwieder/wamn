import "fake-indexeddb/auto";
import { webcrypto } from "node:crypto";
import { afterEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, waitFor, within } from "@solidjs/testing-library";
import { Loading, flush } from "solid-js";
import { outbox, type Change, type Reply } from "@wfe/sync";
import { createSend, createTransport, outboxDatabase, requestHash } from "@wamn/web-runtime";
import { create, PACKAGING_CREATE_ROUTE, type PackagingCreateRequest } from "@wamn/wms-client/packaging.js";
import { packagingCollection, type PackagingRow } from "../src/packaging-feed";
import { packagingAddress } from "../src/packaging-writes";
import { PackagingScreen } from "../src/packaging-screen";
import { makeRows } from "../src/fake";

const disposals: Array<() => void | Promise<void>> = [];
afterEach(async () => {
  cleanup();
  for (const dispose of disposals.splice(0)) await dispose();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

async function mounted() {
  vi.stubGlobal("crypto", webcrypto);
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
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe() {}
      unobserve() {}
      disconnect() {}
    },
  );
  const persisted = vi.fn(async () => true);
  const persist = vi.fn();
  Object.defineProperty(navigator, "storage", { configurable: true, value: { persisted, persist } });
  const changes: Change<PackagingRow>[] = [];
  const c = packagingCollection(
    {
      async list() {
        return { rows: [], cursor: "first" };
      },
      async changes() {
        return { rows: changes.splice(0), next_cursor: "next", complete: true };
      },
    },
    {},
  );
  await c.preload();
  const net = { up: false };
  let answer: Reply = { ok: true };
  const send = vi.fn(async () => answer);
  const o = outbox({ database: `packaging-${crypto.randomUUID()}`, send, probe: async () => net.up });
  await o.replay();
  const view = render(() => (
    <Loading>
      <PackagingScreen collection={c} outbox={o} />
    </Loading>
  ));
  disposals.push(
    () => o.close(),
    () => c.cleanup(),
  );
  await waitFor(() => expect(view.getByRole("form", { name: "Create packaging" })).toBeTruthy());
  const row = (id: string) => view.container.querySelector(`tr[data-row="${id}"]`);
  async function submit(code = "PK-NEW") {
    fireEvent.input(view.getByLabelText("Packaging code"), { target: { value: code } });
    fireEvent.input(view.getByLabelText("Location id"), { target: { value: makeRows(1)[0]!.locationId } });
    fireEvent.submit(view.getByRole("form", { name: "Create packaging" }));
    await waitFor(() => expect(o.entries()).toHaveLength(1));
    flush();
    return o.entries()[0]!;
  }
  return {
    ...view,
    c,
    o,
    row,
    submit,
    changes,
    net,
    send,
    persisted,
    persist,
    answer: (next: Reply) => {
      answer = next;
    },
  };
}

it("takes the project and environment from the application route and the id from the session", () => {
  const route = packagingAddress("/urn:wamn:project-env:acme:wms:dev:instance/live.html");
  expect(route).toEqual({ aud: "urn:wamn:project-env:acme:wms:dev:instance", application: "wms", environment: "dev" });
  expect(outboxDatabase(route.application, route.environment, "signed-in-user")).toBe(
    "wamn-outbox:wms:dev:signed-in-user",
  );
  expect(() => packagingAddress("/live.html")).toThrow("Open /urn:wamn:project-env:");
});

it("records a UUIDv7 create, keeps it tentative after send, and confirms only on the feed echo", async () => {
  const h = await mounted();
  const entry = await h.submit();
  const id = entry.expect.id;
  expect(id).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
  expect(entry.request).toMatchObject({ id, packagingCode: "PK-NEW" });
  expect(entry.request_hash).toBe(await requestHash(entry.request));
  await waitFor(() => expect(h.row(id)?.textContent).toContain("PK-NEW (tentative)"));
  expect(h.getByLabelText("Sync status").textContent).toContain("Pending: 1");
  expect(h.getByLabelText("Sync status").textContent).toContain("Refused: 0");
  expect(h.getByLabelText("Sync status").textContent).toContain("Storage protected");
  expect(h.persisted).toHaveBeenCalledOnce();
  expect(h.persist).not.toHaveBeenCalled();
  h.net.up = true;
  await h.o.replay();
  expect(h.o.entries()).toHaveLength(0);
  flush();
  expect(h.row(id)?.textContent).toContain("tentative");
  expect(h.getByLabelText("Sync status").textContent).not.toContain("Last sent: never");
  const data = { ...makeRows(1)[0]!, ...(entry.request as object), id };
  h.changes.push({ id, row_version: 1, deleted: false, data });
  await h.c.utils.pull();
  await waitFor(() => {
    expect(h.row(id)?.textContent).toContain("PK-NEW");
    expect(h.row(id)?.textContent).not.toContain("tentative");
  });
  await waitFor(() => expect(h.getByLabelText("Sync status").textContent).toContain(h.c.utils.currentAt()!), {
    timeout: 2000,
  });
});

it("shows a refused request beside the current row and discards it", async () => {
  const h = await mounted();
  const entry = await h.submit();
  h.answer({
    ok: false,
    retriable: false,
    refusal: { code: "unique_violation", text: "Packaging code already exists" },
  });
  h.net.up = true;
  await h.o.replay();
  await waitFor(() => expect(h.row(entry.expect.id)).toBeNull());
  const panel = within(h.getByRole("region", { name: "Not uploaded" }));
  expect(panel.getByText("Packaging code already exists")).toBeTruthy();
  expect(panel.getByText("no row")).toBeTruthy();
  expect(panel.getByText(/Operation:/).textContent).toContain(entry.operation);
  expect(panel.getByText(/Record id:/).textContent).toContain(entry.expect.id);
  expect(h.getByLabelText("Sync status").textContent).toContain("Refused: 1");
  const data = { ...makeRows(1)[0]!, id: entry.expect.id, packagingCode: "CURRENT" };
  h.changes.push({ id: data.id, row_version: 1, deleted: false, data });
  await h.c.utils.pull();
  await waitFor(() => expect(panel.getByText(/"packagingCode": "CURRENT"/)).toBeTruthy());
  expect((panel.getByLabelText("Corrected request") as HTMLTextAreaElement).value).toContain("PK-NEW");
  fireEvent.click(panel.getByRole("button", { name: "Discard" }));
  await waitFor(() => expect(h.o.entries()).toHaveLength(0));
  await waitFor(() => expect(panel.queryByText("Packaging code already exists")).toBeNull());
  expect(h.row(data.id)?.textContent).toContain("CURRENT");
});

it("records the edited create with the same id before discarding the refusal, without losing the row", async () => {
  const h = await mounted();
  const old = await h.submit();
  h.answer({ ok: false, retriable: false, refusal: { code: "invalid_input", text: "Correct the packaging code" } });
  h.net.up = true;
  await h.o.replay();
  await waitFor(() => expect(h.getByLabelText("Corrected request")).toBeTruthy());
  h.net.up = false;
  const discard = h.o.discard.bind(h.o);
  let rowBeforeDiscard: Element | null = null;
  const order: string[] = [];
  vi.spyOn(h.o, "discard").mockImplementation(async (seq) => {
    order.push("discard");
    const recorded = h.o.entries();
    expect(recorded).toHaveLength(2);
    expect(recorded[0]!.seq).toBe(old.seq);
    expect(recorded[1]!.expect.id).toBe(old.expect.id);
    expect(recorded[1]!.request_hash).not.toBe(old.request_hash);
    flush();
    rowBeforeDiscard = h.row(old.expect.id);
    expect(rowBeforeDiscard?.textContent).toContain("CORRECTED (tentative)");
    await discard(seq);
    flush();
    expect(h.row(old.expect.id)).toBe(rowBeforeDiscard);
  });
  const stop = h.o.watch(() => {
    if (h.o.entries().length === 2) order.push("recorded");
  });
  fireEvent.input(h.getByLabelText("Corrected request"), {
    target: { value: JSON.stringify({ ...(old.request as object), id: "ignored-edit", packagingCode: "CORRECTED" }) },
  });
  flush();
  fireEvent.click(h.getByRole("button", { name: "Resubmit" }));
  await waitFor(() => expect(h.o.discard).toHaveBeenCalledWith(old.seq));
  await waitFor(() => expect(h.o.entries()).toHaveLength(1));
  expect(order).toEqual(["recorded", "discard"]);
  expect(h.o.entries()[0]!.request).toMatchObject({ id: old.expect.id, packagingCode: "CORRECTED" });
  expect(h.o.entries()[0]!.request_hash).toBe(await requestHash(h.o.entries()[0]!.request));
  expect(h.row(old.expect.id)).toBe(rowBeforeDiscard);
  stop();
  // Discarding the pending replacement uses OutboxDiscarded to remove its overlay.
  await discard(h.o.entries()[0]!.seq);
  await waitFor(() => expect(h.row(old.expect.id)).toBeNull());
});

it("treats a 403 at upload as an ordinary refusal in the panel", async () => {
  const h = await mounted();
  const entry = await h.submit();
  const renew = vi.fn(async () => true);
  const fetch = vi.fn(
    async () =>
      new Response(
        JSON.stringify({
          error: { code: "permission-denied", operation: PACKAGING_CREATE_ROUTE.operation },
        }),
        { status: 403 },
      ),
  );
  const send = createSend<Record<string, PackagingCreateRequest>>({
    transport: createTransport({ baseUrl: "", cookie: true, fetch, cookies: () => "" }),
    bindings: { [PACKAGING_CREATE_ROUTE.operation]: create },
    renew,
  });
  h.send.mockImplementation((..._args) => send(entry, new AbortController().signal));
  h.net.up = true;
  await h.o.replay();
  await waitFor(() => expect(h.o.entries()[0]?.state).toBe("refused"));
  expect(h.o.entries()[0]?.refusal?.code).toBe("permission-denied");
  const panel = h.getByRole("region", { name: "Not uploaded" });
  expect(panel.textContent).toContain(h.o.entries()[0]!.refusal!.text);
  expect(panel.textContent).toContain(entry.expect.id);
  expect(renew).not.toHaveBeenCalled();
  expect(fetch).toHaveBeenCalledOnce();
});

it("keeps the refused entry if recording the correction fails", async () => {
  const h = await mounted();
  const old = await h.submit();
  h.answer({ ok: false, retriable: false, refusal: { code: "invalid_input", text: "Correct the packaging code" } });
  h.net.up = true;
  await h.o.replay();
  await waitFor(() => expect(h.getByLabelText("Corrected request")).toBeTruthy());
  vi.spyOn(h.o, "enqueue").mockRejectedValue(new Error("Storage is full"));
  const discard = vi.spyOn(h.o, "discard");
  fireEvent.input(h.getByLabelText("Corrected request"), {
    target: { value: JSON.stringify({ ...(old.request as object), packagingCode: "CORRECTED" }) },
  });
  flush();
  fireEvent.click(h.getByRole("button", { name: "Resubmit" }));
  await waitFor(() => expect(h.getByRole("alert").textContent).toContain("Storage is full"));
  expect(h.o.entries()).toHaveLength(1);
  expect(h.o.entries()[0]!.seq).toBe(old.seq);
  expect(discard).not.toHaveBeenCalled();
});
