/**
 * A streamed load: its reply lines, its batches and its end (wamn-utci.4).
 */

import { describe, expect, it } from "vitest";

import {
  appendRows,
  type BatchTick,
  emptyLoad,
  endLoad,
  keepLoad,
  readLoadLines,
  startLoad,
} from "../src/load.js";
import { createTransport } from "../src/transport.js";
import type { JsonValue, WireRequest } from "../src/wire.js";

interface Row {
  readonly id: string;
  readonly name: string;
}

const encoder = new TextEncoder();

/** A body that delivers exactly these byte chunks. */
const body = (...chunks: Uint8Array[]): ReadableStream<Uint8Array> =>
  new ReadableStream({
    start(controller) {
      for (const chunk of chunks) {
        controller.enqueue(chunk);
      }
      controller.close();
    },
  });

const text = (...chunks: string[]) => body(...chunks.map((chunk) => encoder.encode(chunk)));

const revive = (row: JsonValue): Row => row as unknown as Row;

/** A tick that hands rows over only when the test flushes. */
const manualTick = () => {
  const due: (() => void)[] = [];
  const tick: BatchTick = (flush) => {
    due.push(flush);
    return () => {
      const at = due.indexOf(flush);
      if (at !== -1) {
        due.splice(at, 1);
      }
    };
  };
  return { tick, due };
};

const row = (id: string, name = id) => `${JSON.stringify({ row: { id, name } })}\n`;
const outcome = (value: JsonValue) => `${JSON.stringify({ outcome: value })}\n`;

describe("the reply lines of a streamed load", () => {
  it("hands the rows over in batches and ends with the outcome line", async () => {
    const batches: (readonly Row[])[] = [];
    const end = await readLoadLines(
      text(row("a"), row("b"), outcome({ value: { more: true }, actor_labels: {} })),
      revive,
      (rows) => batches.push(rows),
    );
    expect(end).toEqual({ status: "completed", value: { more: true } });
    expect(batches).toEqual([[{ id: "a", name: "a" }, { id: "b", name: "b" }]]);
  });

  it("reads a character and a line that a chunk splits", async () => {
    const whole = encoder.encode(`${row("a", "Größe")}${outcome({ value: { more: false } })}`);
    // Split inside the two bytes of "ö", and inside the outcome line.
    const cut = whole.indexOf(0xc3) + 1;
    const second = whole.length - 5;
    const rows: Row[] = [];
    const end = await readLoadLines(
      body(whole.slice(0, cut), whole.slice(cut, second), whole.slice(second)),
      revive,
      (batch) => rows.push(...batch),
    );
    expect(rows).toEqual([{ id: "a", name: "Größe" }]);
    expect(end).toEqual({ status: "completed", value: { more: false } });
  });

  it("fails the load on a malformed line, and never skips it", async () => {
    const rows: Row[] = [];
    const end = await readLoadLines(
      text(row("a"), "{not json}\n", row("b"), outcome({ value: { more: false } })),
      revive,
      (batch) => rows.push(...batch),
    );
    expect(end).toEqual({
      status: "uncertain",
      reason: "the load sent a malformed line",
      retryRefusal: null,
    });
    expect(rows.map((one) => one.id)).toEqual(["a"]);
  });

  it("fails a load whose body ends without its outcome line", async () => {
    const end = await readLoadLines(text(row("a")), revive, () => undefined);
    expect(end.status).toBe("uncertain");
    expect(end.status === "uncertain" && end.reason).toBe("the load ended without its outcome line");
  });

  it("reads a refusal after the first row with the text the operation declares", async () => {
    const end = await readLoadLines(
      text(row("a"), outcome({ error: { code: "timeout", detail: {} } })),
      revive,
      () => undefined,
      [{ literal: "timeout", required: [], sources: [], text: "The load took too long." }],
    );
    expect(end).toEqual({
      status: "refused",
      code: "timeout",
      detail: { detail: {} },
      text: "The load took too long.",
    });
    const uncertain = await readLoadLines(
      text(row("a"), outcome({ uncertain: {} })),
      revive,
      () => undefined,
    );
    expect(uncertain.status).toBe("uncertain");
  });

  it("hands over one batch per tick, never one row at a time", async () => {
    const { tick, due } = manualTick();
    const batches: (readonly Row[])[] = [];
    let release: () => void = () => undefined;
    const held = new Promise<void>((resolve) => {
      release = resolve;
    });
    const stream = new ReadableStream<Uint8Array>({
      async start(controller) {
        controller.enqueue(encoder.encode(row("a") + row("b")));
        await held;
        controller.enqueue(encoder.encode(row("c") + outcome({ value: { more: false } })));
        controller.close();
      },
    });
    const reading = readLoadLines(stream, revive, (rows) => batches.push(rows), [], tick);
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(batches).toEqual([]);
    expect(due).toHaveLength(1);
    due.shift()?.();
    expect(batches.map((batch) => batch.map((one) => one.id))).toEqual([["a", "b"]]);
    release();
    const end = await reading;
    expect(end.status).toBe("completed");
    expect(batches.map((batch) => batch.map((one) => one.id))).toEqual([["a", "b"], ["c"]]);
  });
});

describe("the load state of a streamed load", () => {
  it("keeps the last rows until the first batch, then appends each batch", () => {
    const loaded = endLoad(
      appendRows(startLoad(emptyLoad<Row>(1000)), 1, [{ id: "old", name: "old" }]),
      1,
      { status: "completed", value: { more: false } },
      ["id"],
    );
    const next = startLoad(loaded);
    expect(next.rows.map((one) => one.id)).toEqual(["old"]);
    const first = appendRows(next, 2, [{ id: "a", name: "a" }]);
    const second = appendRows(first, 2, [{ id: "b", name: "b" }]);
    expect(second.rows.map((one) => one.id)).toEqual(["a", "b"]);
    const ended = endLoad(second, 2, { status: "completed", value: { more: true } }, ["id"]);
    expect(ended.busy).toBe(false);
    expect(ended.fullyRead).toBe(false);
    expect(ended.rows.map((one) => one.id)).toEqual(["a", "b"]);
  });

  it("drops the batches and the end of an older load", () => {
    const first = startLoad(emptyLoad<Row>(1000));
    const second = startLoad(first);
    const stale = appendRows(second, first.generation, [{ id: "a", name: "a" }]);
    expect(stale).toBe(second);
    expect(endLoad(second, first.generation, { status: "completed", value: { more: false } }, ["id"])).toBe(
      second,
    );
  });

  it("fails a load that returned one row id twice, and a load with no rows ends empty", () => {
    const loading = appendRows(startLoad(emptyLoad<Row>(1000)), 1, [
      { id: "a", name: "a" },
      { id: "a", name: "again" },
    ]);
    const failed = endLoad(loading, 1, { status: "completed", value: { more: false } }, ["id"]);
    expect(failed.refusal).toBe("The load returned the row id a twice.");
    expect(failed.rows).toEqual([]);
    const loaded = endLoad(
      appendRows(startLoad(emptyLoad<Row>(1000)), 1, [{ id: "x", name: "x" }]),
      1,
      { status: "completed", value: { more: false } },
      ["id"],
    );
    const empty = endLoad(startLoad(loaded), 2, { status: "completed", value: { more: false } }, [
      "id",
    ]);
    expect(empty.rows).toEqual([]);
    expect(empty.fullyRead).toBe(true);
  });
});

describe("an unchanged load", () => {
  it("keeps the rows of the last load and whether it read the whole set", () => {
    const loaded = endLoad(
      appendRows(startLoad(emptyLoad<Row>(1000)), 1, [{ id: "a", name: "a" }]),
      1,
      { status: "completed", value: { more: true } },
      ["id"],
    );
    const again = startLoad(loaded);
    const kept = keepLoad(again, again.generation, loaded.fullyRead);
    expect(kept.rows.map((one) => one.id)).toEqual(["a"]);
    expect(kept.busy).toBe(false);
    expect(kept.fullyRead).toBe(false);
    expect(keepLoad(again, loaded.generation, true)).toBe(again);
  });
});

describe("the transport's streamed read", () => {
  const request: WireRequest = {
    operation: "wamn-wms:pallet/query@1.0.0",
    method: "GET",
    template: "/pallet/query",
    freshOnly: false,
    contract: {
      resultClass: "page",
      partialSchema: null,
      errors: [{ literal: "invalid_input", required: ["field"], sources: [], text: null }],
      replay: null,
      direct: true,
      kind: "query",
      transaction: "implicit",
    },
    items: [{ limit: 1000 }],
  };

  it("asks for the stream shape in the canonical query, and returns the body of lines", async () => {
    const asked: { url: string; init: RequestInit }[] = [];
    const transport = createTransport({
      baseUrl: "https://wms.test",
      credential: "token",
      fetch: async (url, init) => {
        asked.push({ url: String(url), init: init ?? {} });
        return new Response(row("a") + outcome({ value: { more: false } }), {
          status: 200,
          headers: { "content-type": "application/x-ndjson", etag: 'W/"v1"' },
        });
      },
    });
    const opened = await transport.openStream?.(request);
    expect(opened !== undefined && "body" in opened && opened.body).toBeInstanceOf(ReadableStream);
    expect(opened !== undefined && "etag" in opened && opened.etag).toBe('W/"v1"');
    expect(asked[0]?.url).toBe("https://wms.test/pallet/query?limit=1000&shape=%22stream%22");
    expect(asked[0]?.init.cache).toBe("no-store");
    expect((asked[0]?.init.headers as { authorization?: string }).authorization).toBe(
      "Bearer token",
    );
  });

  it("revalidates the last load with its ETag, and unchanged data answers not modified", async () => {
    const asked: RequestInit[] = [];
    const transport = createTransport({
      baseUrl: "https://wms.test",
      fetch: async (_url, init) => {
        asked.push(init ?? {});
        return new Response(null, { status: 304, headers: { etag: 'W/"v1"' } });
      },
    });
    const opened = await transport.openStream?.(request, undefined, 'W/"v1"');
    expect(opened).toEqual({ notModified: true, etag: 'W/"v1"' });
    expect((asked[0]?.headers as { "if-none-match"?: string })["if-none-match"]).toBe('W/"v1"');
  });

  it("classifies a read that ended before its first row as a page reply", async () => {
    const transport = createTransport({
      baseUrl: "https://wms.test",
      fetch: async () =>
        new Response(
          JSON.stringify([{ error: { code: "invalid_input", detail: { field: "limit" } } }]),
          { status: 200, headers: { "content-type": "application/json" } },
        ),
    });
    const opened = await transport.openStream?.(request);
    expect(opened).toEqual({
      status: "refused",
      code: "invalid_input",
      detail: { detail: { field: "limit" } },
    });
  });
});
