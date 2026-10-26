// Run after `cargo run -p wamn-schema-generator --example emit_client_ts` and `pnpm install`.
// From web/sync-bench: npx vitest run --config ../../crates/schema/generator/tests/client_ts_sync.config.mjs
import { expect, it } from "vitest";
import {
  widgetFeed, widgetCollection, widgetKey,
  type WidgetQueryRow,
} from "../../../../web/sync-bench/src/generated/sync_fixture/widget.js";
import type { JsonValue, Transport, WireRequest } from "../../../../web/runtime/src/index.js";

const id = "00000000-0000-7000-8000-000000000001";
const wire = { id, code: "priority", note: null, maker_id: null, edit_version: 4, created_at: "2026-01-01T00:00:00Z" };

it("round-trips widget list pages, changes, signal and cursor expiry", async () => {
  const seen: WireRequest[] = [];
  const signals: (AbortSignal | undefined)[] = [];
  const values: JsonValue[] = [
    { item: [wire], next_cursor: "p2", cursor: "c1" },
    { item: [{ ...wire, id: "second" }], next_cursor: null, cursor: "c2" },
    { rows: [
      { id, row_version: 5, deleted: false, data: { ...wire, edit_version: 5 } },
      { id: "second", row_version: 6, deleted: true },
    ], next_cursor: "c3", complete: false },
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
    [{ ...scope, limit: 100 }], [{ ...scope, limit: 100, cursor: "p2" }],
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
  const feed = widgetFeed({ async invoke() {
    return { status: "completed", value: { item: [wire], next_cursor: "p2", cursor: "c1" } };
  } } as Transport, 1);
  expect(await feed.list({})).toMatchObject({ cursor: "c1", complete: false, rows: [{ id }] });
});

it("uses the declared revision when rejecting a stale update", async () => {
  const row: WidgetQueryRow = { id, code: "priority", note: null, makerId: null, editVersion: 4, createdAt: wire.created_at };
  const rows = [row];
  let next = row;
  const c = widgetCollection({
    async list() { return { rows, cursor: "c1", complete: true }; },
    async changes() { return { rows: [{ id, row_version: next.editVersion, deleted: false, data: next }], next_cursor: "c2", complete: true }; },
  }, {}, { pollMs: 60_000 });
  try {
    await c.preload();
    expect(c.get(id)).toMatchObject(row);
    next = { ...row, editVersion: 3, code: "standard" };
    await c.utils.pull();
    expect(c.get(id)).toMatchObject(row);
    next = { ...row, editVersion: 5, code: "standard" };
    await c.utils.pull();
    expect(c.get(id)).toMatchObject(next);
    expect(widgetKey({ filter: { code: ["priority"], note: ["a"] } })).toBe(widgetKey({ filter: { note: ["a"], code: ["priority"] } }));
  } finally {
    await c.cleanup();
  }
});
