// Run after `cargo run -p wamn-compiler --example emit_client_ts` and `pnpm install`.
// From web/sync-bench: pnpm exec vitest run --config ../../crates/schema/generator/tests/client_ts_sync.config.mjs
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
import {
  screenCollection,
  screenFeed,
  screenKey,
  FEED_LINE_SCREEN_ROUTE,
} from "../../../../web/sync-bench/src/generated/sync_fixture/feed_line.js";
import {
  screenCollection as lineCollection,
  screenFeed as lineFeed,
} from "../../../../web/sync-bench/src/generated/sync_line_key/feed_line.js";
import type { Int64, JsonValue, Transport, WireRequest } from "../../../../web/runtime/src/index.js";

const id = "00000000-0000-7000-8000-000000000001";
const wire = {
  row_version: "4000",
  id,
  code: "priority",
  note: null,
  maker_id: null,
  edit_version: 4,
  created_at: "2026-01-01T00:00:00Z",
};

it("round-trips widget list pages, changes, signal and cursor expiry", async () => {
  const seen: WireRequest[] = [];
  const signals: (AbortSignal | undefined)[] = [];
  const values: JsonValue[] = [
    { item: [wire], next_cursor: "p2", cursor: "c1" },
    { item: [{ ...wire, id: "second" }], next_cursor: null, cursor: "c2" },
    {
      rows: [
        { id, row_version: "4001", deleted: false, data: { ...wire, row_version: "4001", edit_version: 5 } },
        { id: "second", row_version: "4001", deleted: true },
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
  const target = { filter: { code: ["priority"] } };
  const page = await feed.list(target, signal);
  {
const observed = page;
expect(observed).toHaveProperty(["cursor"], "c1");
expect(observed).toHaveProperty(["complete"], true);
}
  expect(page.rows).toHaveLength(2);
  {
const observed = page.rows[0];
expect(observed).toHaveProperty(["id"], id);
expect(observed).toHaveProperty(["editVersion"], 4);
}
  expect(seen.map((request) => request.items)).toEqual([
    [{ ...target, limit: 100 }],
    [{ ...target, limit: 100, cursor: "p2" }],
  ]);
  const changes = await feed.changes(target, page.cursor, 50, signal);
  expect(seen[2]?.items).toEqual([{ ...target, feed: { cursor: "c1", limit: 50 } }]);
  {
const observed = changes;
expect(observed).toHaveProperty(["next_cursor"], "c3");
expect(observed).toHaveProperty(["complete"], false);
}
  {
const observed = changes.rows[0];
expect(observed).toHaveProperty(["id"], id);
expect(observed).toHaveProperty(["row_version"], 4001);
expect(observed).toHaveProperty(["data","rowVersion"], "4001");
expect(observed).toHaveProperty(["data","editVersion"], 5);
}
  {
const observed = changes.rows[1];
expect(observed).toHaveProperty(["id"], "second");
expect(observed).toHaveProperty(["row_version"], 4001);
expect(observed).toHaveProperty(["deleted"], true);
}
  {
const observed = signals;
expect(observed).toHaveProperty(["length"], 3);
expect(observed).toHaveProperty(["0"], signal);
expect(observed).toHaveProperty(["1"], signal);
expect(observed).toHaveProperty(["2"], signal);
}
  await expect(feed.changes(target, "expired")).rejects.toMatchObject({ name: "CursorExpired" });
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
  {
const observed = await feed.list({});
expect(observed).toHaveProperty(["cursor"], "c1");
expect(observed).toHaveProperty(["complete"], false);
expect(observed).toHaveProperty(["rows","length"], 1);
expect(observed).toHaveProperty(["rows","0","id"], id);
}
});

it("lists an authored view, follows an equal snapshot and deletes by the root id", async () => {
  const seen: WireRequest[] = [];
  const signals: (AbortSignal | undefined)[] = [];
  const target = { orderId: id };
  const line = {
    id,
    order_id: id,
    item_id: "item-1",
    label: "line",
    order_label: "order",
    item_label: "item",
    row_version: "4000",
  };
  const values: JsonValue[] = [
    { item: [line, { ...line, id: "second" }], next_cursor: null, cursor: "c1" },
    {
      rows: [{ id, row_version: "4000", deleted: false, data: { ...line, order_label: "updated" } }],
      next_cursor: "c2",
      complete: true,
    },
    { rows: [{ id, row_version: "4001", deleted: true }], next_cursor: "c3", complete: true },
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
  const feed = screenFeed(transport, 1);
  const c = screenCollection(feed, target, { pollMs: 60_000 });
  try {
    await c.preload();
    expect(c.size).toBe(2);
    expect(c.utils.complete()).toBe(true);
    {
const observed = c.get(id);
expect(observed).toHaveProperty(["id"], id);
expect(observed).toHaveProperty(["orderLabel"], "order");
expect(observed).toHaveProperty(["itemLabel"], "item");
expect(observed).toHaveProperty(["rowVersion"], "4000");
}
    await c.utils.pull();
    expect(c.get(id)?.orderLabel).toBe("updated");
    await c.utils.pull();
    expect(c.has(id)).toBe(false);
    expect(c.has("second")).toBe(true);
    expect(seen.map((request) => request.operation)).toEqual(Array(3).fill(FEED_LINE_SCREEN_ROUTE.operation));
    {
const observed = seen.map((request) => request.items);
expect(observed).toHaveProperty(["length"], 3);
expect(observed).toHaveProperty(["0","length"], 1);
expect(observed).toHaveProperty(["0","0","order_id"], id);
expect(observed).toHaveProperty(["1","length"], 1);
expect(observed).toHaveProperty(["1","0","order_id"], id);
expect(observed).toHaveProperty(["1","0","feed","cursor"], "c1");
expect(observed).toHaveProperty(["2","length"], 1);
expect(observed).toHaveProperty(["2","0","order_id"], id);
expect(observed).toHaveProperty(["2","0","feed","cursor"], "c2");
}
    expect(signals.every((signal) => signal instanceof AbortSignal)).toBe(true);
    expect(screenKey(target)).not.toBe(screenKey({ orderId: "other" }));
    const signal = new AbortController().signal;
    await expect(feed.changes(target, "expired", 50, signal)).rejects.toMatchObject({ name: "CursorExpired" });
    {
const observed = seen[3]?.items;
expect(observed).toHaveProperty(["length"], 1);
expect(observed).toHaveProperty(["0","order_id"], id);
expect(observed).toHaveProperty(["0","feed","cursor"], "expired");
expect(observed).toHaveProperty(["0","feed","limit"], 50);
}
    expect(signals[3]).toBe(signal);
  } finally {
    await c.cleanup();
  }
});

it("lists, follows and deletes a collection keyed on line_id", async () => {
  const line = {
    line_id: id,
    order_id: id,
    item_id: "item-1",
    label: "line",
    order_label: "open",
    item_label: "item",
    row_version: "4000",
  };
  const values: JsonValue[] = [
    { item: [line], next_cursor: null, cursor: "c1" },
    {
      rows: [{ id, row_version: "4000", deleted: false, data: { ...line, order_label: "updated" } }],
      next_cursor: "c2",
      complete: true,
    },
    { rows: [{ id, row_version: "4001", deleted: true }], next_cursor: "c3", complete: true },
  ];
  const transport = {
    async invoke() {
      return { status: "completed", value: values.shift()! };
    },
  } as Transport;
  const c = lineCollection(lineFeed(transport), { orderId: id }, { pollMs: 60_000 });
  try {
    await c.preload();
    expect(c.get(id)).toEqual(expect.objectContaining({ lineId: id, orderLabel: "open" }));
    expect(c.get(id)).not.toHaveProperty("id");
    await c.utils.pull();
    expect(c.get(id)).toEqual(expect.objectContaining({ lineId: id, orderLabel: "updated" }));
    await c.utils.pull();
    expect(c.size).toBe(0);
  } finally {
    await c.cleanup();
  }
});

it("replaces equal snapshots and ignores lower snapshots for pulls and refills", async () => {
  const row: WidgetQueryRow = {
    id,
    code: "priority",
    note: null,
    makerId: null,
    editVersion: 4,
    rowVersion: "4003",
    createdAt: wire.created_at,
  };
  let rows = [row];
  let next = row;
  let batch: WidgetQueryRow[] | undefined;
  const c = widgetCollection(
    {
      async list() {
        return { rows, cursor: "c1", complete: true };
      },
      async changes() {
        return {
          rows: (batch ?? [next]).map((data) => ({
            id,
            row_version: Number(data.rowVersion),
            deleted: false as const,
            data,
          })),
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
    next = { ...row, rowVersion: "4002", code: "standard" };
    await c.utils.pull();
    expect(c.get(id)).toMatchObject(row);
    next = { ...row, code: "standard" };
    await c.utils.pull();
    expect(c.get(id)).toMatchObject(next);
    rows = [{ ...next, note: "equal refill" }];
    await c.utils.refill();
    expect(c.get(id)?.note).toBe("equal refill");
    rows = [{ ...row, rowVersion: "4002" }];
    await c.utils.refill();
    expect(c.get(id)?.note).toBe("equal refill");
    next = { ...row, rowVersion: "4004", note: "newer" };
    await c.utils.pull();
    expect(c.get(id)).toMatchObject(next);
    batch = [
      { ...row, rowVersion: "4005", note: "held in batch" },
      { ...row, rowVersion: "4004", note: "lower in batch" },
    ];
    await c.utils.pull();
    expect(c.get(id)?.note).toBe("held in batch");
    expect(widgetKey({ filter: { code: ["priority"], note: ["a"] } })).toBe(
      widgetKey({ filter: { note: ["a"], code: ["priority"] } }),
    );
  } finally {
    await c.cleanup();
  }
});

const counterId: Int64 = "9007199254740992";
const nextCounterId: Int64 = "9007199254740993";
const counterWire = {
  row_version: "4000",
  id: counterId,
  label: "first",
  edit_version: 1,
  created_at: wire.created_at,
};

it("fills counter pages and renders changes by their exact Int64 ids", async () => {
  const seen: WireRequest[] = [];
  const values: JsonValue[] = [
    { item: [counterWire], next_cursor: "p2", cursor: "c1" },
    { item: [{ ...counterWire, id: nextCounterId, label: "second" }], next_cursor: null, cursor: "c2" },
    {
      rows: [
        {
          id: counterId,
          row_version: "4001",
          deleted: false,
          data: { ...counterWire, row_version: "4001", label: "updated", edit_version: 2 },
        },
        { id: nextCounterId, row_version: "4001", deleted: true },
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
    {
const observed = seen.map((request) => request.items);
expect(observed).toHaveProperty(["length"], 2);
expect(observed).toHaveProperty(["0","length"], 1);
expect(observed).toHaveProperty(["0","0","limit"], 100);
expect(observed).toHaveProperty(["1","length"], 1);
expect(observed).toHaveProperty(["1","0","limit"], 100);
expect(observed).toHaveProperty(["1","0","cursor"], "p2");
}
    await c.utils.pull();
    {
const observed = seen[2]?.items;
expect(observed).toHaveProperty(["length"], 1);
expect(observed).toHaveProperty(["0","feed","cursor"], "c1");
}
    {
const observed = c.get(counterId);
expect(observed).toHaveProperty(["id"], counterId);
expect(observed).toHaveProperty(["label"], "updated");
expect(observed).toHaveProperty(["editVersion"], 2);
}
    expect(c.has(nextCounterId)).toBe(false);
    await vi.waitFor(() => expect(contents()).toEqual([`${counterId}:updated;`, `${counterId}:updated;`]));
  } finally {
    view?.unmount();
    await c.cleanup();
  }
});

it("refuses an offline counter create and sends an online create without an outbox entry", async () => {
  let online = false;
  const invoke = vi.fn<Transport["invoke"]>(async () => ({
    status: "completed",
    value: { id: counterId, label: "first", edit_version: 1, created_at: wire.created_at },
  }));
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
    await expect(record.create(items, options)).rejects.toThrow();
    expect(probe).toHaveBeenCalledWith(options.signal);
    expect(invoke).not.toHaveBeenCalled();
    expect(enqueue).not.toHaveBeenCalled();
    {
const observed = o.entries();
expect(observed).toHaveProperty(["length"], 0);
}
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
    {
const observed = o.entries();
expect(observed).toHaveProperty(["length"], 0);
}
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
    const row: CounterRow = {
      rowVersion: "4000",
      id: counterId,
      label: "first",
      editVersion: 1,
      createdAt: wire.created_at,
    };
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
      {
const observed = o.entries()[0];
expect(observed).toHaveProperty(["expect","id"], counterId);
expect(observed).toHaveProperty(["expected"], expected);
}
      await vi.waitFor(() => expect(view.container.textContent).toBe("updated"));
      expect(invoke).not.toHaveBeenCalled();
      await o.discard(o.entries()[0]!.seq);
      {
const observed = await result;
expect(observed).toHaveProperty(["name"], "OutboxDiscarded");
}
      await vi.waitFor(() => expect(view.container.textContent).toBe("first"));
    } finally {
      view.unmount();
      o.close();
      await c.cleanup();
    }
  },
);
