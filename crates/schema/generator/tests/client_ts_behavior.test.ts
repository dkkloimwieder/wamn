import { createRequire } from "node:module";
import { join } from "node:path";
import { expect, it, vi } from "vitest";
import * as client from "@fixture/fixture/index.ts";
import input from "@fixture/fixture/input.json";
import * as hidden from "@fixture/hidden/widget.ts";
import * as noFeed from "@fixture/no_feed/widget.ts";
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
      expect(invoke).toHaveBeenCalledExactlyOnceWith(
        {
          operation: operation.operation,
          method: operation.route.method,
          template: operation.route.template,
          contract: expect.any(Object),
          items: [],
        },
        options,
      );
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
  expect(await widget.list({ invoke }, [{ makerId: "maker-1", selector: json }])).toEqual(
    completed({
      rows: [{ id: wire.id, code: wire.code, attributes: json, editVersion: 4 }],
    }),
  );
  {
const observed = invoke.mock.calls[0][0].items;
expect(observed).toHaveProperty(["length"], 1);
expect(observed).toHaveProperty(["0","maker_id"], "maker-1");
expect(observed).toHaveProperty(["0","selector"], json);
}
  const query = vi.fn(async () => completed({ item: [wire], next_cursor: null, cursor: "c1" }));
  expect(
    await widget.query({ invoke: query }, [
      { filter: { makerId: ["maker-1"] }, sort: { field: "created_at", direction: "ascending" }, limit: 2 },
    ]),
  ).toEqual(completed({ item: [row], nextCursor: null, cursor: "c1" }));
  {
const observed = query.mock.calls[0][0].items;
expect(observed).toHaveProperty(["length"], 1);
expect(observed).toHaveProperty(["0","filter","maker_id","length"], 1);
expect(observed).toHaveProperty(["0","filter","maker_id","0"], "maker-1");
expect(observed).toHaveProperty(["0","sort","field"], "created_at");
expect(observed).toHaveProperty(["0","sort","direction"], "ascending");
expect(observed).toHaveProperty(["0","limit"], 2);
}
  const batch = vi.fn(async () => completed({ widget_id: "widget-1" }));
  expect(
    await widget.recordBatch({ invoke: batch }, [
      {
        requestId: "r",
        value: { expectedEditVersion: "9007199254740993", line: [{ widgetId: "widget-1", amount: "2" }] },
      },
    ]),
  ).toEqual(completed({ widgetId: "widget-1" }));
  {
const observed = batch.mock.calls[0][0].items;
expect(observed).toHaveProperty(["length"], 1);
expect(observed).toHaveProperty(["0","request_id"], "r");
expect(observed).toHaveProperty(["0","value","expected_edit_version"], "9007199254740993");
expect(observed).toHaveProperty(["0","value","line","length"], 1);
expect(observed).toHaveProperty(["0","value","line","0","widget_id"], "widget-1");
expect(observed).toHaveProperty(["0","value","line","0","amount"], "2");
}
  for (const outcome of [
    { status: "partiallyCompleted", committedResult: wire, failedOutcome: json },
    { status: "refused", code: "invalid_input", detail: json },
    { status: "uncertain", reason: "lost", retryRefusal: json },
  ]) {
    const result = await widget.get({ invoke: async () => outcome }, [{ id: wire.id }]);
    expect(result).toEqual(outcome.status === "partiallyCompleted" ? { ...outcome, committedResult: row } : outcome);
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
  expect(await feed.list({}, signal)).toEqual({
    rows: [row, { ...row, id: "widget-2" }],
    cursor: "c1",
    complete: true,
  });
  expect(await feed.changes({}, "c1", 50, signal)).toEqual({
    rows: [
      { id: wire.id, row_version: 5, deleted: false, data: { ...row, editVersion: 5 } },
      { id: "widget-2", row_version: 6, deleted: true },
    ],
    next_cursor: "c3",
    complete: false,
  });
  {
const observed = invoke.mock.calls.map(([request]) => request.items);
expect(observed).toHaveProperty(["length"], 3);
expect(observed).toHaveProperty(["0","length"], 1);
expect(observed).toHaveProperty(["0","0","limit"], 100);
expect(observed).toHaveProperty(["1","length"], 1);
expect(observed).toHaveProperty(["1","0","limit"], 100);
expect(observed).toHaveProperty(["1","0","cursor"], "p2");
expect(observed).toHaveProperty(["2","length"], 1);
expect(observed).toHaveProperty(["2","0","feed","cursor"], "c1");
expect(observed).toHaveProperty(["2","0","feed","limit"], 50);
}
  {
const observed = invoke.mock.calls.map(([, options]) => options.signal);
expect(observed).toHaveProperty(["length"], 3);
expect(observed).toHaveProperty(["0"], signal);
expect(observed).toHaveProperty(["1"], signal);
expect(observed).toHaveProperty(["2"], signal);
}
  await expect(feed.changes({}, "expired")).rejects.toMatchObject({ name: "CursorExpired" });
  const capped = widget.widgetFeed(
    { invoke: async () => completed({ item: [wire], next_cursor: "p2", cursor: "c1" }) },
    1,
  );
  {
const observed = await capped.list({});
expect(observed).toHaveProperty(["rows","length"], 1);
expect(observed).toHaveProperty(["rows","0"], row);
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
    expect(recordWrite).toHaveBeenLastCalledWith(c, outbox, { model: "widget" });
    const options = { signal: new AbortController().signal };
    const items = [{ code: "priority", requestId: "r", idempotencyKey: "k" }];
    await expect(writer.create(items, options)).rejects.toThrow();
    expect(transport.probe).toHaveBeenCalledWith(options.signal);
    expect(invoke).not.toHaveBeenCalled();
    transport.probe.mockResolvedValue(true);
    expect(await writer.create(items, options)).toEqual(completed({ ...row, id }));
    expect(invoke).toHaveBeenCalledExactlyOnceWith(
      { ...module.WIDGET_CREATE_ROUTE, items: [{ code: "priority", request_id: "r", idempotency_key: "k" }] },
      options,
    );
    expect(invoke.mock.calls[0][0].items[0]).not.toHaveProperty("id");
  }
});

it("omits unserved bindings and opt-out feeds while keeping target keys stable", () => {
  for (const name of ["get", "WIDGET_GET_ROUTE", "widgetArchived", "WIDGET_WIDGET_ARCHIVED_ROUTE"]) {
    expect(hidden).not.toHaveProperty(name);
  }
  {
const observed = hidden.WIDGET_GET_REQUEST_FIELDS;
expect(observed).toHaveProperty(["id"], "id");
}
  for (const name of ["changes", "widgetFeed", "widgetCollection", "widgetWrite"])
    expect(noFeed).not.toHaveProperty(name);
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

it("exposes authored descriptions through TypeScript symbols", () => {
  const require = createRequire(new URL("../../../../package.json", import.meta.url));
  const ts = require("typescript");
  const file = join(process.env.WAMN_CLIENT_TS_TEST_ROOT, "fixture/widget.ts");
  const program = ts.createProgram([file], {});
  const types = program.getTypeChecker();
  const source = program.getSourceFile(file);
  const symbols = types.getExportsOfModule(types.getSymbolAtLocation(source));
  const symbol = (name) => symbols.find((symbol) => symbol.name === name);
  const docs = (symbol) => ts.displayPartsToString(symbol.getDocumentationComment(types));
  const member = (name, field) => types.getDeclaredTypeOfSymbol(symbol(name)).getProperty(field);
  expect(docs(symbol("WidgetRecordBatchRequest"))).toBe(
    "Input for `platform-fixture:widget/record-batch@2.1.1`.\n\nOne submission that records several lines at once.",
  );
  expect(docs(member("WidgetRecordBatchRequestValue", "note"))).toBe(
    "What the operator recorded about this batch.\n\n`text`, omittable",
  );
  expect(docs(member("WidgetRecordBatchRequestValue", "line"))).toBe(
    "One line for each widget this batch records.\n\n`array`",
  );
  expect(docs(member("WidgetGetRequest", "id"))).toBe("`uuid`");
  expect(docs(symbol("WidgetGetRequest"))).toBe("Input for `platform-fixture:widget/get@2.1.1`.");
  expect(docs(member("WidgetGetResult", "code"))).toBe("The code an operator types to find one widget.\n\n`text`");
});
