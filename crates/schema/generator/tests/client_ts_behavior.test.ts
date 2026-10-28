import { createRequire } from "node:module";
import { join } from "node:path";
import { expect, it, vi } from "vitest";
import * as client from "@fixture/fixture/index.ts";
import * as hidden from "@fixture/hidden/widget.ts";
import * as noFeed from "@fixture/no_feed/widget.ts";
import * as int32 from "@fixture/int32/widget.ts";
import * as int64 from "@fixture/int64/widget.ts";
import { collection, recordWrite } from "@wfe/sync";

vi.mock("@wfe/sync", async (original) => ({
  ...(await original()),
  collection: vi.fn((_feed, _scope, options) => ({ options })),
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
  expect(Object.keys(client).sort()).toEqual([
    "counter",
    "feedLine",
    "number",
    "numberedLabel",
    "widget",
    "widgetMaker",
    "widgetTag",
  ]);
  const options = { signal: new AbortController().signal };
  for (const [operation, name] of [
    ["archive", "archive"],
    ["create", "create"],
    ["delete", "delete_"],
    ["get", "get"],
    ["list", "list"],
    ["query", "query"],
    ["record-batch", "recordBatch"],
    ["update", "update"],
  ]) {
    const invoke = vi.fn(async () => completed(null));
    await widget[name]({ invoke }, [], options);
    expect(invoke).toHaveBeenCalledExactlyOnceWith(
      {
        operation: `platform-fixture:widget/${operation}@2.1.0`,
        method: ["get", "list", "query"].includes(operation) ? "GET" : "POST",
        template: `/widget/${operation.replaceAll("-", "_")}`,
        contract: expect.any(Object),
        items: [],
      },
      options,
    );
    const contract = invoke.mock.calls[0][0].contract;
    expect(contract).not.toHaveProperty("baseUrl");
    if (["get", "list", "query"].includes(operation)) {
      expect(contract.reads).toEqual(["inventory.widget"]);
      expect(contract.writes).toBeNull();
    }
    if (["create", "update", "delete"].includes(operation)) {
      expect(contract.reads).toEqual([]);
      expect(contract.writes).toEqual(["inventory.widget"]);
    }
    if (operation === "archive") expect(contract.replay).toBe("state");
    if (operation === "query") expect(contract.resultClass).toBe("page");
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
  expect(invoke.mock.calls[0][0].items).toEqual([{ maker_id: "maker-1", selector: json }]);
  const query = vi.fn(async () => completed({ item: [wire], next_cursor: null, cursor: "c1" }));
  expect(
    await widget.query({ invoke: query }, [
      { filter: { makerId: ["maker-1"] }, sort: { field: "created_at", direction: "ascending" }, limit: 2 },
    ]),
  ).toEqual(completed({ item: [row], nextCursor: null, cursor: "c1" }));
  expect(query.mock.calls[0][0].items).toEqual([
    { filter: { maker_id: ["maker-1"] }, sort: { field: "created_at", direction: "ascending" }, limit: 2 },
  ]);
  const batch = vi.fn(async () => completed({ widget_id: "widget-1" }));
  expect(
    await widget.recordBatch({ invoke: batch }, [
      {
        requestId: "r",
        value: { expectedEditVersion: "9007199254740993", line: [{ widgetId: "widget-1", amount: "2" }] },
      },
    ]),
  ).toEqual(completed({ widgetId: "widget-1" }));
  expect(batch.mock.calls[0][0].items).toEqual([
    {
      request_id: "r",
      value: { expected_edit_version: "9007199254740993", line: [{ widget_id: "widget-1", amount: "2" }] },
    },
  ]);
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
  expect(invoke.mock.calls.map(([request]) => request.items)).toEqual([
    [{ limit: 100 }],
    [{ limit: 100, cursor: "p2" }],
    [{ feed: { cursor: "c1", limit: 50 } }],
  ]);
  expect(invoke.mock.calls.map(([, options]) => options.signal)).toEqual([signal, signal, signal]);
  await expect(feed.changes({}, "expired")).rejects.toMatchObject({ name: "CursorExpired" });
  const capped = widget.widgetFeed(
    { invoke: async () => completed({ item: [wire], next_cursor: "p2", cursor: "c1" }) },
    1,
  );
  expect(await capped.list({})).toEqual({ rows: [row], cursor: "c1", complete: false });
  await expect(widget.widgetFeed({ invoke: async () => ({ status: "uncertain" }) }).list({})).rejects.toThrow(
    "widget query uncertain",
  );
});

it("preserves integer keys and refuses offline creates before dispatch", async () => {
  for (const [module, id] of [
    [int32, 7],
    [int64, "9007199254740993"],
  ]) {
    const scope = { filter: { code: ["priority"] } };
    const feed = module.widgetFeed({
      invoke: async () => completed({ item: [{ ...wire, id }], next_cursor: null, cursor: "c1" }),
    });
    expect((await feed.list(scope)).rows[0].id).toBe(id);
    const c = module.widgetCollection(feed, scope);
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
    await expect(writer.create(items, options)).rejects.toThrow("This record cannot be created offline.");
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

it("omits unserved bindings and opt-out feeds while keeping scope keys stable", () => {
  for (const name of ["get", "WIDGET_GET_ROUTE", "widgetArchived", "WIDGET_WIDGET_ARCHIVED_ROUTE"]) {
    expect(hidden).not.toHaveProperty(name);
  }
  expect(hidden.WIDGET_GET_REQUEST_FIELDS).toEqual({ id: "id" });
  for (const name of ["changes", "widgetFeed", "widgetCollection", "widgetWrite"])
    expect(noFeed).not.toHaveProperty(name);
  expect(widget.widgetKey({ filter: { code: ["priority"], note: ["a"] } })).toBe(
    widget.widgetKey({ filter: { note: ["a"], code: ["priority"] } }),
  );
  expect(widget.widgetKey({ filter: { code: ["priority"] } })).not.toBe(
    widget.widgetKey({ filter: { code: ["standard"] } }),
  );
  expect(JSON.parse(widget.widgetKey({}))).toEqual(["widget", widget.WIDGET_SHAPE_DIGEST, "{}"]);
});

it("exposes authored descriptions through TypeScript symbols", () => {
  const require = createRequire(new URL("../../../../package.json", import.meta.url));
  const ts = require("typescript");
  const file = join(process.env.WAMN_CLIENT_TS_TEST_ROOT, "fixture/widget.ts");
  const program = ts.createProgram([file], {});
  const checker = program.getTypeChecker();
  const source = program.getSourceFile(file);
  const symbols = checker.getExportsOfModule(checker.getSymbolAtLocation(source));
  const symbol = (name) => symbols.find((symbol) => symbol.name === name);
  const docs = (symbol) => ts.displayPartsToString(symbol.getDocumentationComment(checker));
  const member = (name, field) => checker.getDeclaredTypeOfSymbol(symbol(name)).getProperty(field);
  expect(docs(symbol("WidgetRecordBatchRequest"))).toBe(
    "Input for `platform-fixture:widget/record-batch@2.1.0`.\n\nOne submission that records several lines at once.",
  );
  expect(docs(member("WidgetRecordBatchRequestValue", "note"))).toBe(
    "What the operator recorded about this batch.\n\n`text`, omittable",
  );
  expect(docs(member("WidgetRecordBatchRequestValue", "line"))).toBe(
    "One line for each widget this batch records.\n\n`array`",
  );
  expect(docs(member("WidgetGetRequest", "id"))).toBe("`uuid`");
  expect(docs(symbol("WidgetGetRequest"))).toBe("Input for `platform-fixture:widget/get@2.1.0`.");
  expect(docs(member("WidgetGetResult", "code"))).toBe("The code an operator types to find one widget.\n\n`text`");
});
