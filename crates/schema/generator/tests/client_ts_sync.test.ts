// Run after `cargo run -p wamn-schema-generator --example emit_client_ts` and `pnpm install`.
// From web/sync-bench: npx vitest run --config ../../crates/schema/generator/tests/client_ts_sync.config.mjs
import "fake-indexeddb/auto";
import { expect, it, vi } from "vitest";
import { render } from "@solidjs/testing-library";
import { createRenderEffect, Loading } from "solid-js";
import { liveList, liveRows, outbox } from "@wfe/sync";
import {
  counterCollection,
  counterFeed,
  counterWrite,
  COUNTER_CREATE_ROUTE,
  COUNTER_DELETE_ROUTE,
  COUNTER_UPDATE_ROUTE,
  type CounterRow,
} from "../../../../web/sync-bench/src/generated/sync_fixture/counter.js";
import {
  widgetFeed,
  widgetCollection,
  widgetKey,
  type WidgetQueryRow,
} from "../../../../web/sync-bench/src/generated/sync_fixture/widget.js";
import type { Int64, JsonValue, Transport, WireRequest } from "../../../../web/runtime/src/index.js";

const id = "00000000-0000-7000-8000-000000000001";
const wire = { id, code: "priority", note: null, maker_id: null, edit_version: 4, created_at: "2026-01-01T00:00:00Z" };

it("round-trips widget list pages, changes, signal and cursor expiry", async () => {
  const seen: WireRequest[] = [];
  const signals: (AbortSignal | undefined)[] = [];
  const values: JsonValue[] = [
    { item: [wire], next_cursor: "p2", cursor: "c1" },
    { item: [{ ...wire, id: "second" }], next_cursor: null, cursor: "c2" },
    {
      rows: [
        { id, row_version: 5, deleted: false, data: { ...wire, edit_version: 5 } },
        { id: "second", row_version: 6, deleted: true },
      ],
      next_cursor: "c3",
      complete: false,
    },
  ];
  const transport = {
    async invoke(request, options) {
      seen.push(request);
      signals.push(options?.signal);
      const value = values.shift();
      return value === undefined
        ? { status: "refused", code: "cursor_expired", detail: null }
        : { status: "completed", value };
    },
  } as Transport;
  const signal = new AbortController().signal;
  const feed = widgetFeed(transport);
  const scope = { filter: { code: ["priority"] } };
  const page = await feed.list(scope, signal);
  expect(page).toMatchObject({ cursor: "c1", complete: true });
  expect(page.rows).toHaveLength(2);
  expect(page.rows[0]).toMatchObject({ id, editVersion: 4 });
  expect(seen.map((request) => request.items)).toEqual([
    [{ ...scope, limit: 100 }],
    [{ ...scope, limit: 100, cursor: "p2" }],
  ]);
  const changes = await feed.changes(scope, page.cursor, 50, signal);
  expect(seen[2]?.items).toEqual([{ ...scope, feed: { cursor: "c1", limit: 50 } }]);
  expect(changes).toMatchObject({ next_cursor: "c3", complete: false });
  expect(changes.rows[0]).toMatchObject({ id, row_version: 5, data: { editVersion: 5 } });
  expect(changes.rows[1]).toEqual({ id: "second", row_version: 6, deleted: true });
  expect(signals).toEqual([signal, signal, signal]);
  await expect(feed.changes(scope, "expired")).rejects.toMatchObject({ name: "CursorExpired" });
});

it("reports an incomplete widget list at the cap", async () => {
  const feed = widgetFeed(
    {
      async invoke() {
        return { status: "completed", value: { item: [wire], next_cursor: "p2", cursor: "c1" } };
      },
    } as Transport,
    1,
  );
  expect(await feed.list({})).toMatchObject({ cursor: "c1", complete: false, rows: [{ id }] });
});

it("uses the declared revision when rejecting a stale update", async () => {
  const row: WidgetQueryRow = {
    id,
    code: "priority",
    note: null,
    makerId: null,
    editVersion: 4,
    createdAt: wire.created_at,
  };
  const rows = [row];
  let next = row;
  const c = widgetCollection(
    {
      async list() {
        return { rows, cursor: "c1", complete: true };
      },
      async changes() {
        return {
          rows: [{ id, row_version: next.editVersion, deleted: false, data: next }],
          next_cursor: "c2",
          complete: true,
        };
      },
    },
    {},
    { pollMs: 60_000 },
  );
  try {
    await c.preload();
    expect(c.get(id)).toMatchObject(row);
    next = { ...row, editVersion: 3, code: "standard" };
    await c.utils.pull();
    expect(c.get(id)).toMatchObject(row);
    next = { ...row, editVersion: 5, code: "standard" };
    await c.utils.pull();
    expect(c.get(id)).toMatchObject(next);
    expect(widgetKey({ filter: { code: ["priority"], note: ["a"] } })).toBe(
      widgetKey({ filter: { note: ["a"], code: ["priority"] } }),
    );
  } finally {
    await c.cleanup();
  }
});

const counterId: Int64 = "9007199254740992";
const nextCounterId: Int64 = "9007199254740993";
const counterWire = { id: counterId, label: "first", edit_version: 1, created_at: wire.created_at };

it("fills counter pages and renders changes by their exact Int64 ids", async () => {
  const seen: WireRequest[] = [];
  const values: JsonValue[] = [
    { item: [counterWire], next_cursor: "p2", cursor: "c1" },
    { item: [{ ...counterWire, id: nextCounterId, label: "second" }], next_cursor: null, cursor: "c2" },
    {
      rows: [
        { id: counterId, row_version: 2, deleted: false, data: { ...counterWire, label: "updated", edit_version: 2 } },
        { id: nextCounterId, row_version: 2, deleted: true },
      ],
      next_cursor: "c3",
      complete: true,
    },
  ];
  const transport = {
    async invoke(request) {
      seen.push(request);
      return { status: "completed", value: values.shift()! };
    },
  } as Transport;
  const c = counterCollection(counterFeed(transport), {}, { pollMs: 60_000 });
  let view: ReturnType<typeof render> | undefined;
  try {
    view = render(
      () => {
        const rows = liveRows(c);
        const list = liveList(c);
        const host = document.createElement("div");
        const mapNode = document.createElement("div");
        const listNode = document.createElement("div");
        mapNode.dataset.map = "";
        listNode.dataset.list = "";
        host.append(mapNode, listNode);
        createRenderEffect(
          () => [
            Object.values(rows)
              .map((row) => `${row.id}:${row.label};`)
              .join(""),
            list.map((row) => `${row.id}:${row.label};`).join(""),
          ],
          ([map, list]) => {
            mapNode.textContent = map!;
            listNode.textContent = list!;
          },
        );
        return host;
      },
      { wrapper: Loading },
    );
    const contents = () =>
      [...view!.container.querySelectorAll("[data-map], [data-list]")].map((node) => node.textContent);
    const initial = `${counterId}:first;${nextCounterId}:second;`;
    await vi.waitFor(() => expect(contents()).toEqual([initial, initial]));
    expect(c.get(counterId)?.label).toBe("first");
    expect(c.get(nextCounterId)?.label).toBe("second");
    expect(c.get(Number(counterId))).toBeUndefined();
    expect(seen.map((request) => request.items)).toEqual([[{ limit: 100 }], [{ limit: 100, cursor: "p2" }]]);
    await c.utils.pull();
    expect(seen[2]?.items).toEqual([{ feed: { cursor: "c1" } }]);
    expect(c.get(counterId)).toMatchObject({ id: counterId, label: "updated", editVersion: 2 });
    expect(c.has(nextCounterId)).toBe(false);
    await vi.waitFor(() => expect(contents()).toEqual([`${counterId}:updated;`, `${counterId}:updated;`]));
  } finally {
    view?.unmount();
    await c.cleanup();
  }
});

it("refuses an offline counter create and sends an online create without an outbox entry", async () => {
  let online = false;
  const invoke = vi.fn<Transport["invoke"]>(async () => ({ status: "completed", value: counterWire }));
  const probe = vi.fn(async (_signal?: AbortSignal) => online);
  const transport: Transport = { invoke, probe };
  const c = counterCollection(
    {
      async list() {
        return { rows: [], cursor: "c1" };
      },
      async changes() {
        return { rows: [], next_cursor: "c1", complete: true };
      },
    },
    {},
    { pollMs: 60_000 },
  );
  const send = vi.fn(async () => ({ ok: true as const }));
  const o = outbox<Int64>({ database: "counter-create", probe: async () => false, send });
  const enqueue = vi.spyOn(o, "enqueue");
  let record!: ReturnType<typeof counterWrite>;
  const view = render(() => {
    record = counterWrite(c, o, transport);
    return document.createElement("div");
  });
  try {
    const items = [{ label: "first", idempotencyKey: "create-1", requestId: "request-1" }];
    const options = { signal: new AbortController().signal };
    await expect(record.create(items, options)).rejects.toThrow("This record cannot be created offline.");
    expect(probe).toHaveBeenCalledWith(options.signal);
    expect(invoke).not.toHaveBeenCalled();
    expect(enqueue).not.toHaveBeenCalled();
    expect(o.entries()).toEqual([]);
    online = true;
    await expect(record.create(items, options)).resolves.toEqual({
      status: "completed",
      value: {
        id: counterId,
        label: "first",
        editVersion: 1,
        createdAt: wire.created_at,
      },
    });
    expect(invoke).toHaveBeenCalledExactlyOnceWith(
      {
        ...COUNTER_CREATE_ROUTE,
        items: [
          {
            label: "first",
            idempotency_key: "create-1",
            request_id: "request-1",
          },
        ],
      },
      options,
    );
    expect(enqueue).not.toHaveBeenCalled();
    expect(o.entries()).toEqual([]);
    expect(send).not.toHaveBeenCalled();
  } finally {
    view.unmount();
    o.close();
    await c.cleanup();
  }
});

it.each([COUNTER_UPDATE_ROUTE, COUNTER_DELETE_ROUTE])(
  "records $operation through recordWrite with the Int64 id",
  async (route) => {
    const row: CounterRow = { id: counterId, label: "first", editVersion: 1, createdAt: wire.created_at };
    const c = counterCollection(
      {
        async list() {
          return { rows: [row], cursor: "c1" };
        },
        async changes() {
          return { rows: [], next_cursor: "c1", complete: true };
        },
      },
      {},
      { pollMs: 60_000 },
    );
    const invoke = vi.fn<Transport["invoke"]>();
    const o = outbox<Int64>({
      database: `counter-${route.operation}`,
      probe: async () => false,
      send: async () => ({ ok: true }),
    });
    const enqueue = vi.spyOn(o, "enqueue");
    let record!: ReturnType<typeof counterWrite>;
    await c.preload();
    const view = render(
      () => {
        record = counterWrite(c, o, { invoke, probe: async () => false });
        const host = document.createElement("div");
        createRenderEffect(
          () => record.rows[counterId]?.label,
          (label) => {
            host.textContent = label ?? "";
          },
        );
        return host;
      },
      { wrapper: Loading },
    );
    try {
      await vi.waitFor(() => expect(view.container.textContent).toBe("first"));
      const expected = { ...row, label: "updated" };
      const call = {
        operation: route.operation,
        request: { id: counterId },
        request_hash: "write-1",
        expect: { model: "counter", id: counterId },
        append_only: false,
      };
      const result = record.write(call, expected).catch((error: unknown) => error);
      await vi.waitFor(() => expect(o.entries()).toHaveLength(1));
      expect(enqueue).toHaveBeenCalledExactlyOnceWith(call, expected);
      expect(o.entries()[0]).toMatchObject({ expect: { id: counterId }, expected });
      await vi.waitFor(() => expect(view.container.textContent).toBe("updated"));
      expect(invoke).not.toHaveBeenCalled();
      await o.discard(o.entries()[0]!.seq);
      expect(await result).toMatchObject({ name: "OutboxDiscarded" });
      await vi.waitFor(() => expect(view.container.textContent).toBe("first"));
    } finally {
      view.unmount();
      o.close();
      await c.cleanup();
    }
  },
);
