import { expect, it, vi } from "vitest";
import * as client from "@fixture/fixture/index.ts";
import input from "@fixture/fixture/input.json";
import * as hidden from "@fixture/hidden/widget.ts";
import hiddenInput from "@fixture/hidden/input.json";
import * as noFeed from "@fixture/no_feed/widget.ts";
import noFeedInput from "@fixture/no_feed/input.json";
import * as int32 from "@fixture/int32/widget.ts";
import * as int64 from "@fixture/int64/widget.ts";
import { collection, recordWrite } from "@wfe/sync";

vi.mock("@wfe/sync", async (original) => ({
  ...(await original()),
  collection: vi.fn((_feed, _target, options) => ({ options })),
  recordWrite: vi.fn(() => ({ write: "outbox write" })),
}));

const widget = client.widget;
const wire = {
  id: "widget-1",
  code: "priority",
  maker_id: null,
  note: null,
  edit_version: 4,
  created_at: "2026-01-01T00:00:00Z",
};
const row = { id: wire.id, code: wire.code, makerId: null, note: null, editVersion: 4, createdAt: wire.created_at };
const completed = (value) => ({ status: "completed", value });

it("invokes every public operation with its route, effects and abort options", async () => {
  const member = (name) => name.replace(/_([a-z])/g, (_, letter) => letter.toUpperCase());
  expect(Object.keys(client).sort()).toEqual(input.models.map((model) => member(model.name)).sort());
  const options = { signal: new AbortController().signal };
  for (const model of input.models) {
    for (const operation of model.operations) {
      if (operation.type === "event_handler" || !operation.route) continue;
      const name = operation.name === "delete" ? "delete_" : member(operation.name);
      const invoke = vi.fn(async () => completed(null));
      await client[member(model.name)][name]({ invoke }, [], options);
      expect(invoke).toHaveBeenCalledTimes(1);
      const [request, observedOptions] = invoke.mock.calls[0];
      expect(request.operation).toBe(operation.operation);
      expect(request.method).toBe(operation.route.method);
      expect(request.template).toBe(operation.route.template);
      expect(request.contract).toBeTypeOf("object");
      expect(request.items).toHaveLength(0);
      expect(observedOptions).toBe(options);
      const contract = invoke.mock.calls[0][0].contract;
      expect(contract).not.toHaveProperty("baseUrl");
      expect(contract.reads).toEqual(operation.route.response.reads ?? []);
      expect(contract.writes).toEqual(operation.route.response.writes ?? null);
      expect(contract.replay).toBe(operation.route.replay ?? null);
      expect(contract.resultClass).toBe(operation.route.response["result-class"] ?? null);
    }
  }
});

it("maps declared members and envelopes while keeping JSON and refusal data intact", async () => {
  const json = { snake_key: { mixedCase: [{ nested_key: 1 }] } };
  const invoke = vi.fn(async () =>
    completed({ rows: [{ id: wire.id, code: wire.code, attributes: json, edit_version: 4 }] }),
  );
  const listed = await widget.list({ invoke }, [{ makerId: "maker-1", selector: json }]);
  expect(listed.status).toBe("completed");
  expect(listed.value.rows).toHaveLength(1);
  expect(listed.value.rows[0].id).toBe(wire.id);
  expect(listed.value.rows[0].code).toBe(wire.code);
  expect(listed.value.rows[0].attributes).toEqual(json);
  expect(listed.value.rows[0].editVersion).toBe(wire.edit_version);
  {
    const observed = invoke.mock.calls[0][0].items;
    expect(observed).toHaveProperty(["length"], 1);
    expect(observed).toHaveProperty(["0", "maker_id"], "maker-1");
    expect(observed).toHaveProperty(["0", "selector"], json);
  }
  const query = vi.fn(async () => completed({ item: [wire], next_cursor: null, cursor: "c1" }));
  const queried = await widget.query({ invoke: query }, [
    { filter: { makerId: ["maker-1"] }, sort: { field: "created_at", direction: "ascending" }, limit: 2 },
  ]);
  expect(queried.status).toBe("completed");
  expect(queried.value.item).toHaveLength(1);
  for (const [field, value] of Object.entries(row)) expect(queried.value.item[0][field]).toEqual(value);
  expect(queried.value.nextCursor).toBeNull();
  expect(queried.value.cursor).toBe("c1");
  {
    const observed = query.mock.calls[0][0].items;
    expect(observed).toHaveProperty(["length"], 1);
    expect(observed).toHaveProperty(["0", "filter", "maker_id", "length"], 1);
    expect(observed).toHaveProperty(["0", "filter", "maker_id", "0"], "maker-1");
    expect(observed).toHaveProperty(["0", "sort", "field"], "created_at");
    expect(observed).toHaveProperty(["0", "sort", "direction"], "ascending");
    expect(observed).toHaveProperty(["0", "limit"], 2);
  }
  const batch = vi.fn(async () => completed({ widget_id: "widget-1" }));
  const recorded = await widget.recordBatch({ invoke: batch }, [
    {
      requestId: "r",
      value: { expectedEditVersion: "9007199254740993", line: [{ widgetId: "widget-1", amount: "2" }] },
    },
  ]);
  expect(recorded.status).toBe("completed");
  expect(recorded.value.widgetId).toBe("widget-1");
  {
    const observed = batch.mock.calls[0][0].items;
    expect(observed).toHaveProperty(["length"], 1);
    expect(observed).toHaveProperty(["0", "request_id"], "r");
    expect(observed).toHaveProperty(["0", "value", "expected_edit_version"], "9007199254740993");
    expect(observed).toHaveProperty(["0", "value", "line", "length"], 1);
    expect(observed).toHaveProperty(["0", "value", "line", "0", "widget_id"], "widget-1");
    expect(observed).toHaveProperty(["0", "value", "line", "0", "amount"], "2");
  }
  for (const outcome of [
    { status: "partiallyCompleted", committedResult: wire, failedOutcome: json },
    { status: "refused", code: "invalid_input", detail: json },
    { status: "uncertain", reason: "lost", retryRefusal: json },
  ]) {
    const result = await widget.get({ invoke: async () => outcome }, [{ id: wire.id }]);
    expect(result.status).toBe(outcome.status);
    if (outcome.status === "partiallyCompleted") {
      for (const [field, value] of Object.entries(row)) expect(result.committedResult[field]).toEqual(value);
      expect(result.failedOutcome).toEqual(outcome.failedOutcome);
    } else if (outcome.status === "refused") {
      expect(result.code).toBe(outcome.code);
      expect(result.detail).toEqual(outcome.detail);
    } else {
      expect(result.reason).toBe(outcome.reason);
      expect(result.retryRefusal).toEqual(outcome.retryRefusal);
    }
  }
});

it("follows pages and changes with the first cursor, cap and cancellation signal", async () => {
  const values = [
    completed({ item: [wire], next_cursor: "p2", cursor: "c1" }),
    completed({ item: [{ ...wire, id: "widget-2" }], next_cursor: null, cursor: "c2" }),
    completed({
      rows: [
        { id: wire.id, row_version: 5, deleted: false, data: { ...wire, edit_version: 5 } },
        { id: "widget-2", row_version: 6, deleted: true },
      ],
      next_cursor: "c3",
      complete: false,
    }),
    { status: "refused", code: "cursor_expired", detail: null },
  ];
  const invoke = vi.fn(async () => values.shift());
  const signal = new AbortController().signal;
  const feed = widget.widgetFeed({ invoke });
  const page = await feed.list({}, signal);
  expect(page.rows).toHaveLength(2);
  expect(page.rows[0]).toEqual(row);
  expect(page.rows[1].id).toBe("widget-2");
  expect(page.rows[1].code).toBe(row.code);
  expect(page.rows[1].makerId).toBe(row.makerId);
  expect(page.rows[1].note).toBe(row.note);
  expect(page.rows[1].editVersion).toBe(row.editVersion);
  expect(page.rows[1].createdAt).toBe(row.createdAt);
  expect(page.cursor).toBe("c1");
  expect(page.complete).toBe(true);
  const changes = await feed.changes({}, "c1", 50, signal);
  expect(changes.rows).toHaveLength(2);
  expect(changes.rows[0].id).toBe(wire.id);
  expect(changes.rows[0].row_version).toBe(5);
  expect(changes.rows[0].deleted).toBe(false);
  expect(changes.rows[0].data.id).toBe(row.id);
  expect(changes.rows[0].data.code).toBe(row.code);
  expect(changes.rows[0].data.makerId).toBe(row.makerId);
  expect(changes.rows[0].data.note).toBe(row.note);
  expect(changes.rows[0].data.editVersion).toBe(5);
  expect(changes.rows[0].data.createdAt).toBe(row.createdAt);
  expect(changes.rows[1].id).toBe("widget-2");
  expect(changes.rows[1].row_version).toBe(6);
  expect(changes.rows[1].deleted).toBe(true);
  expect(changes.next_cursor).toBe("c3");
  expect(changes.complete).toBe(false);
  {
    const observed = invoke.mock.calls.map(([request]) => request.items);
    expect(observed).toHaveProperty(["length"], 3);
    expect(observed).toHaveProperty(["0", "length"], 1);
    expect(observed).toHaveProperty(["0", "0", "limit"], 100);
    expect(observed).toHaveProperty(["1", "length"], 1);
    expect(observed).toHaveProperty(["1", "0", "limit"], 100);
    expect(observed).toHaveProperty(["1", "0", "cursor"], "p2");
    expect(observed).toHaveProperty(["2", "length"], 1);
    expect(observed).toHaveProperty(["2", "0", "feed", "cursor"], "c1");
    expect(observed).toHaveProperty(["2", "0", "feed", "limit"], 50);
  }
  {
    const observed = invoke.mock.calls.map(([, options]) => options.signal);
    expect(observed).toHaveProperty(["length"], 3);
    expect(observed).toHaveProperty(["0"], signal);
    expect(observed).toHaveProperty(["1"], signal);
    expect(observed).toHaveProperty(["2"], signal);
  }
  await expect(feed.changes({}, "expired")).rejects.toHaveProperty("name", "CursorExpired");
  const capped = widget.widgetFeed(
    { invoke: async () => completed({ item: [wire], next_cursor: "p2", cursor: "c1" }) },
    1,
  );
  {
    const observed = await capped.list({});
    expect(observed).toHaveProperty(["rows", "length"], 1);
    expect(observed).toHaveProperty(["rows", "0"], row);
    expect(observed).toHaveProperty(["cursor"], "c1");
    expect(observed).toHaveProperty(["complete"], false);
  }
  await expect(widget.widgetFeed({ invoke: async () => ({ status: "uncertain" }) }).list({})).rejects.toThrow();
});

it("preserves integer keys and refuses offline creates before dispatch", async () => {
  for (const [module, id] of [
    [int32, 7],
    [int64, "9007199254740993"],
  ]) {
    const target = { filter: { code: ["priority"] } };
    const feed = module.widgetFeed({
      invoke: async () => completed({ item: [{ ...wire, id }], next_cursor: null, cursor: "c1" }),
    });
    expect((await feed.list(target)).rows[0].id).toBe(id);
    const c = module.widgetCollection(feed, target);
    const configuration = vi.mocked(collection).mock.lastCall[2];
    expect(configuration.getKey({ id })).toBe(id);
    expect(configuration.version({ rowVersion: "8" })).toBe(8);
    const outbox = {};
    const invoke = vi.fn(async () => completed({ ...wire, id }));
    const transport = { invoke, probe: vi.fn(async () => false) };
    const writer = module.widgetWrite(c, outbox, transport);
    expect(vi.mocked(recordWrite).mock.lastCall[0]).toBe(c);
    expect(vi.mocked(recordWrite).mock.lastCall[1]).toBe(outbox);
    expect(vi.mocked(recordWrite).mock.lastCall[2].model).toBe("widget");
    const options = { signal: new AbortController().signal };
    const items = [{ code: "priority", requestId: "r", idempotencyKey: "k" }];
    await expect(writer.create(items, options)).rejects.toThrow();
    expect(transport.probe).toHaveBeenCalledWith(options.signal);
    expect(invoke).not.toHaveBeenCalled();
    transport.probe.mockResolvedValue(true);
    const created = await writer.create(items, options);
    expect(created.status).toBe("completed");
    expect(created.value.id).toBe(id);
    for (const [field, value] of Object.entries(row)) {
      if (field !== "id") expect(created.value[field]).toEqual(value);
    }
    expect(invoke).toHaveBeenCalledTimes(1);
    const [request, observedOptions] = invoke.mock.calls[0];
    for (const [field, value] of Object.entries(module.WIDGET_CREATE_ROUTE)) expect(request[field]).toEqual(value);
    expect(request.items).toHaveLength(1);
    expect(request.items[0].code).toBe(items[0].code);
    expect(request.items[0].request_id).toBe(items[0].requestId);
    expect(request.items[0].idempotency_key).toBe(items[0].idempotencyKey);
    expect(observedOptions).toBe(options);
    expect(invoke.mock.calls[0][0].items[0]).not.toHaveProperty("id");
  }
});

it("omits unserved bindings and opt-out feeds while keeping target keys stable", () => {
  const member = (name) => name.replace(/_([a-z])/g, (_, letter) => letter.toUpperCase());
  const model = hiddenInput.models.find((model) => model.name === "widget");
  for (const operation of model.operations) {
    if (operation.route && operation.type !== "event_handler") continue;
    const name = operation.name === "delete" ? "delete_" : member(operation.name);
    expect(hidden).not.toHaveProperty(name);
    expect(hidden).not.toHaveProperty(`${model.name}_${operation.name}_ROUTE`.toUpperCase());
  }
  {
    const observed = hidden.WIDGET_GET_REQUEST_FIELDS;
    expect(observed).toHaveProperty(["id"], "id");
  }
  const withoutFeeds = noFeedInput.models.find((candidate) => candidate.name === model.name);
  for (const operation of input.models.find((model) => model.name === "widget").operations) {
    if (!operation.sync) continue;
    expect(withoutFeeds.operations.find((candidate) => candidate.name === operation.name).sync).not.toBe(true);
    const prefix = member(operation.type === "query" ? withoutFeeds.name : operation.name);
    for (const suffix of ["Feed", "Collection", "Write"]) expect(noFeed).not.toHaveProperty(prefix + suffix);
    const changes = operation.type === "query" ? "changes" : member(operation.name) + "Changes";
    expect(noFeed).not.toHaveProperty(changes);
  }
  expect(widget.widgetKey({ filter: { code: ["priority"], note: ["a"] } })).toBe(
    widget.widgetKey({ filter: { note: ["a"], code: ["priority"] } }),
  );
  expect(widget.widgetKey({ filter: { code: ["priority"] } })).not.toBe(
    widget.widgetKey({ filter: { code: ["standard"] } }),
  );
  {
    const observed = JSON.parse(widget.widgetKey({}));
    expect(observed).toHaveProperty(["length"], 3);
    expect(observed).toHaveProperty(["0"], "widget");
    expect(observed).toHaveProperty(["1"], widget.WIDGET_SHAPE_DIGEST);
    expect(observed).toHaveProperty(["2"], "{}");
  }
});
