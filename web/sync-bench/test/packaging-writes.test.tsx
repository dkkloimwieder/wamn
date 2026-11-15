import "fake-indexeddb/auto";
import { webcrypto } from "node:crypto";
import { afterEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, waitFor, within } from "@solidjs/testing-library";
import { Loading, flush } from "solid-js";
import { outbox, shell, type Change, type OperationCall, type Reply, type Outbox, type OutboxCounts } from "@wfe/sync";
import {
  createCall,
  createSend,
  createTransport,
  outboxDatabase,
  requestHash,
  rowsPersistence,
} from "@wamn/web-runtime";
import {
  create,
  PACKAGING_CREATE_REQUEST_FIELDS,
  PACKAGING_CREATE_ROUTE,
  PACKAGING_SHAPE_DIGEST,
  type PackagingCreateRequest,
} from "../src/generated/wamn_wms/packaging.js";
import { packagingCollection, packagingKey, type PackagingRow } from "../src/generated/wamn_wms/packaging.js";
import { packagingAddress } from "../src/packaging-writes.js";
import { PackagingScreen } from "../src/packaging-screen.js";
import { makeRows } from "../src/fake.js";

const disposals: Array<() => void | Promise<void>> = [];
afterEach(async () => {
  cleanup();
  for (const dispose of disposals.splice(0)) await dispose();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

async function mounted(restored?: { call: OperationCall; row: PackagingRow; list: Promise<never> }) {
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
  const persist = vi.fn(async () => true);
  const estimate = vi.fn(async () => ({ usage: 1024, quota: 4096 }));
  Object.defineProperty(navigator, "storage", { configurable: true, value: { persisted, persist, estimate } });
  const shellResult = await shell();
  const changes: Change<PackagingRow>[] = [];
  let readFailure: Error | undefined;
  const persistence = rowsPersistence(`rows-${crypto.randomUUID()}`, [PACKAGING_SHAPE_DIGEST]);
  if (restored) await persistence.save(packagingKey({}), [restored.row], "saved");
  const c = packagingCollection(
    {
      async list() {
        if (restored) return restored.list;
        return { rows: [], cursor: "first" };
      },
      async changes() {
        if (readFailure) throw readFailure;
        return { rows: changes.splice(0), next_cursor: "next", complete: true };
      },
    },
    {},
    { persistence },
  );
  await c.preload();
  const net = { up: false };
  let answer: Reply = { ok: true };
  const send = vi.fn(async () => answer);
  const database = `packaging-${crypto.randomUUID()}`;
  if (restored) {
    const previous = outbox({ database, send, probe: async () => false });
    await previous.enqueue(restored.call, { ...(restored.call.request as object), $tentative: true });
    await previous.replay();
    previous.close();
  }
  const o = outbox({ database, send, probe: async () => net.up });
  await o.replay();
  const view = render(() => (
    <Loading>
      <PackagingScreen collection={c} outbox={o} shellResult={shellResult} />
    </Loading>
  ));
  disposals.push(
    () => o.close(),
    () => c.cleanup(),
  );
  await waitFor(() => expect(view.getByRole("form", { name: "Create packaging" })).toBeTruthy());
  const row = (id: string) => view.container.querySelector(`tr[data-row="${id}"]`);
  async function submit(code = "PK-NEW") {
    const count = o.entries().length;
    fireEvent.input(view.getByLabelText("Packaging code"), { target: { value: code } });
    fireEvent.input(view.getByLabelText("Location id"), { target: { value: makeRows(1)[0]!.locationId } });
    fireEvent.submit(view.getByRole("form", { name: "Create packaging" }));
    await waitFor(() => expect(o.entries()).toHaveLength(count + 1));
    flush();
    return o.entries().at(-1)!;
  }
  return {
    ...view,
    c,
    o,
    shellResult,
    row,
    submit,
    changes,
    failRead: (error: Error | undefined) => {
      readFailure = error;
    },
    net,
    send,
    persisted,
    persist,
    answer: (next: Reply) => {
      answer = next;
    },
  };
}

function counts(o: Outbox): OutboxCounts {
  const observed: OutboxCounts[] = [];
  o.watch((value) => observed.push(value))();
  return observed[0]!;
}

it("takes the project and environment from the application route and the id from the session", () => {
  const route = packagingAddress("/urn:wamn:project-env:acme:wms:dev:instance/live.html");
  {
    const observed = route;
    expect(observed).toHaveProperty(["aud"], "urn:wamn:project-env:acme:wms:dev:instance");
    expect(observed).toHaveProperty(["application"], "wms");
    expect(observed).toHaveProperty(["environment"], "dev");
  }
  expect(outboxDatabase(route.application, route.environment, "signed-in-user")).toBe(
    "wamn-outbox:wms:dev:signed-in-user",
  );
  expect(() => packagingAddress("/live.html")).toThrow();
});

it("enables repeated offline creates before any upload", async () => {
  const h = await mounted();
  const reset = vi.spyOn(HTMLFormElement.prototype, "reset");
  const form = h.getByRole("form", { name: "Create packaging" });
  for (let count = 1; count <= 3; count++) {
    const entry = await h.submit(`OFFLINE-${count}`);
    await waitFor(() => expect(reset).toHaveBeenCalledTimes(count));
    expect((within(form).getByRole("button", { name: "Create packaging" }) as HTMLButtonElement).disabled).toBe(false);
    {
      expect(h.row(entry.expect.id)?.textContent).toContain(`OFFLINE-${count}`);
      expect(h.row(entry.expect.id)?.textContent).toContain("tentative");
    }
  }
  {
    const observed = h.o.entries().map((entry) => entry.state);
    expect(observed).toHaveProperty(["length"], 3);
    expect(observed).toHaveProperty(["0"], "pending");
    expect(observed).toHaveProperty(["1"], "pending");
    expect(observed).toHaveProperty(["2"], "pending");
  }
  expect(h.send).not.toHaveBeenCalled();
});

it("records a UUIDv7 create, keeps it tentative after send, and confirms after a later complete follow pass", async () => {
  const h = await mounted();
  const entry = await h.submit();
  const id = entry.expect.id;
  expect(id).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
  {
    const observed = entry.request;
    expect(observed).toHaveProperty(["id"], id);
    expect(observed).toHaveProperty(["packagingCode"], "PK-NEW");
  }
  expect(entry.request_hash).toBe(await requestHash(entry.request));
  await waitFor(() => {
    expect(h.row(id)?.textContent).toContain("PK-NEW");
    expect(h.row(id)?.textContent).toContain("tentative");
  });
  expect(counts(h.o).pending).toBe(1);
  expect(counts(h.o).refused).toBe(0);
  expect(h.shellResult.persisted).toBe(true);
  expect(h.shellResult.worker).toBe(false);
  expect(h.shellResult.quota.quota).toBe(4096);
  expect(h.shellResult.quota.usage).toBe(1024);
  expect(h.persisted).not.toHaveBeenCalled();
  expect(h.persist).toHaveBeenCalledOnce();
  h.net.up = true;
  await h.o.replay();
  expect(h.o.entries()).toHaveLength(0);
  flush();
  expect(h.row(id)?.textContent).toContain("tentative");
  expect(Number.isNaN(Date.parse(counts(h.o).lastSentAt!))).toBe(false);
  const data = { ...makeRows(1)[0]!, ...(entry.request as object), id };
  h.changes.push({ id, row_version: 1, deleted: false, data });
  await h.c.utils.pull();
  await waitFor(() => {
    expect(h.row(id)?.textContent).toContain("PK-NEW");
    expect(h.row(id)?.textContent).not.toContain("tentative");
  });
  // An empty feed reply updates the time without a row change or a refresh timer.
  vi.useFakeTimers({ toFake: ["Date"] });
  try {
    const at = "2026-10-26T12:00:00.000Z";
    vi.setSystemTime(new Date(at));
    await h.c.utils.pull();
    flush();
    expect(h.getByLabelText("Sync status").textContent).toContain(at);
  } finally {
    vi.useRealTimers();
  }
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
  expect(panel.getByText(/Operation:/).textContent).toContain(entry.operation);
  expect(panel.getByText(/Record id:/).textContent).toContain(entry.expect.id);
  expect(counts(h.o).refused).toBe(1);
  const data = { ...makeRows(1)[0]!, id: entry.expect.id, packagingCode: "CURRENT" };
  h.changes.push({ id: data.id, row_version: 1, deleted: false, data });
  await h.c.utils.pull();
  await waitFor(() => {
    const current = panel.getByRole("heading", { name: "Current row" }).nextElementSibling;
    expect(JSON.parse(current!.textContent!)).toHaveProperty("packagingCode", data.packagingCode);
  });
  expect(JSON.parse((panel.getByLabelText("Corrected request") as HTMLTextAreaElement).value)).toHaveProperty(
    "packagingCode",
    "PK-NEW",
  );
  fireEvent.click(panel.getByRole("button", { name: "Discard" }));
  await waitFor(() => expect(h.o.entries()).toHaveLength(0));
  await waitFor(() => expect(panel.queryByText("Packaging code already exists")).toBeNull());
  expect(h.row(data.id)?.textContent).toContain("CURRENT");
});

it("waits through a failed follow pass and confirms without a result row", async () => {
  const h = await mounted();
  const entry = await h.submit();
  const id = entry.expect.id;
  h.net.up = true;
  await h.o.replay();
  h.failRead(new Error("offline"));
  await h.c.utils.pull();
  flush();
  expect(h.row(id)?.textContent).toContain("tentative");
  h.failRead(undefined);
  await h.c.utils.pull();
  await waitFor(() => expect(h.row(id)).toBeNull());
});

it("keeps a create tentative when its feed echo arrives before the successful retry", async () => {
  const h = await mounted();
  const entry = await h.submit("LOST-REPLY");
  h.answer({ ok: false, retriable: true });
  h.net.up = true;
  await h.o.replay();
  expect(h.send).toHaveBeenCalledOnce();
  expect(counts(h.o).pending).toBe(1);
  const id = entry.expect.id;
  const data = { ...makeRows(1)[0]!, ...(entry.request as object), id };
  h.changes.push({ id, row_version: 1, deleted: false, data });
  await h.c.utils.pull();
  await waitFor(() => {
    expect(h.row(id)?.textContent).toContain("LOST-REPLY");
    expect(h.row(id)?.textContent).toContain("tentative");
  });
  expect(h.o.entries()).toHaveLength(1);
  expect(h.o.entries()[0]?.state).toBe("pending");
  expect(h.send).toHaveBeenCalledOnce();
  h.answer({ ok: true });
  await h.o.replay();
  flush();
  expect(h.row(id)?.textContent).toContain("tentative");
  await h.c.utils.pull();
  await waitFor(() => expect(h.row(id)?.textContent).not.toContain("tentative"));
  expect(h.send).toHaveBeenCalledTimes(2);
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
    {
      expect(rowBeforeDiscard?.textContent).toContain("CORRECTED");
      expect(rowBeforeDiscard?.textContent).toContain("tentative");
    }
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
  {
    const observed = order;
    expect(observed).toHaveProperty(["length"], 2);
    expect(observed).toHaveProperty(["0"], "recorded");
    expect(observed).toHaveProperty(["1"], "discard");
  }
  {
    const observed = h.o.entries()[0]!.request;
    expect(observed).toHaveProperty(["id"], old.expect.id);
    expect(observed).toHaveProperty(["packagingCode"], "CORRECTED");
  }
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
    bindings: { [PACKAGING_CREATE_ROUTE.operation]: { invoke: create, fields: PACKAGING_CREATE_REQUEST_FIELDS } },
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
  await waitFor(() => expect(h.getByRole("alert").textContent?.trim()).toBeTruthy());
  expect(h.o.entries()).toHaveLength(1);
  expect(h.o.entries()[0]!.seq).toBe(old.seq);
  expect(discard).not.toHaveBeenCalled();
});

it("paints saved rows and recovered tentative creates before the list resolves, then waits for send and follow", async () => {
  const row = makeRows(1)[0]!;
  const { call, id } = await createCall<PackagingCreateRequest>({
    operation: PACKAGING_CREATE_ROUTE.operation,
    model: "packaging",
    request: { packagingCode: "RECOVERED", locationId: row.locationId, type: "pallet", status: "available" },
    append_only: true,
  });
  let failList!: (reason: Error) => void;
  const list = new Promise<never>((_resolve, reject) => {
    failList = reject;
  });
  const h = await mounted({ call, row, list });
  await waitFor(() => expect(h.row(row.id)?.textContent).toContain(row.packagingCode));
  await waitFor(() => {
    expect(h.row(id)?.textContent).toContain("RECOVERED");
    expect(h.row(id)?.textContent).toContain("tentative");
  });
  expect(h.send).not.toHaveBeenCalled();
  expect(h.c.utils.currentAt()).toBeUndefined();
  failList(new Error("offline"));
  h.failRead(new Error("offline"));
  await h.c.utils.pull();
  await waitFor(() => expect(h.row(row.id)?.textContent).toContain(row.packagingCode));
  {
    expect(h.row(id)?.textContent).toContain("RECOVERED");
    expect(h.row(id)?.textContent).toContain("tentative");
  }
  h.failRead(undefined);
  h.changes.push({ id, row_version: 1, deleted: false, data: { ...row, ...(call.request as object), id } });
  await h.c.utils.pull();
  await waitFor(() => expect(h.row(id)?.textContent).toContain("tentative"));
  h.net.up = true;
  await h.o.replay();
  flush();
  expect(h.row(id)?.textContent).toContain("tentative");
  await h.c.utils.pull();
  await waitFor(() => expect(h.row(id)?.textContent).not.toContain("tentative"));
  expect(h.send).toHaveBeenCalledOnce();
  h.changes.push({ id, row_version: 2, deleted: true });
  await h.c.utils.pull();
  await waitFor(() => expect(h.row(id)).toBeNull());
});
