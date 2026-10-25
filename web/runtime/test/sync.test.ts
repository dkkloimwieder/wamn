import type { OutboxEntry, Reply } from "@wfe/sync";
import { describe, expect, it, vi } from "vitest";

import { createSend, outboxDatabase, requestHash } from "../src/sync/index.js";
import { createTransport } from "../src/transport.js";
import { reviveOutcome, toWire, type OperationRoute, type Transport } from "../src/wire.js";

const operation = "fixture:row/create@1.0.0";
const route: OperationRoute = {
  operation,
  method: "POST",
  template: "/row/create",
  contract: {
    resultClass: "one",
    partialSchema: null,
    replay: "claim",
    direct: true,
    type: "create",
    transaction: "explicit_per_input",
    reads: [],
    writes: ["fixture.row"],
    errors: [
      { literal: "retry", required: [], sources: ["connection_unavailable", "serialization_failure"], text: null },
      { literal: "timeout", required: [], sources: ["statement_timeout"], text: null },
      { literal: "internal_error", required: [], sources: ["query_error", "row_limit_exceeded"], text: null },
      {
        literal: "unique_violation",
        required: ["constraint"],
        sources: ["unique_violation"],
        text: "This row already exists.",
      },
      { literal: "check_violation", required: ["constraint"], sources: ["check_violation"], text: null },
    ],
  },
};

/** A binding with the same signature and conversion as the generated create binding. */
async function create(
  transport: Transport,
  items: readonly {
    idempotencyKey: string;
    requestId: string;
    label: string;
  }[],
) {
  return reviveOutcome<{ id: string }>(
    await transport.invoke({
      ...route,
      items: items.map((item) =>
        toWire(item, {
          idempotency_key: "idempotencyKey",
          request_id: "requestId",
          label: "label",
        }),
      ),
    }),
    { id: "id" },
  );
}

function entry(): OutboxEntry {
  return {
    operation,
    request: { label: "Box" },
    request_hash: "stored-hash",
    expect: { model: "row", id: "row-1" },
    append_only: false,
    seq: 1,
    recorded_at: "2026-10-25T00:00:00Z",
    state: "pending",
  };
}

function harness(answer: (requestId: string, attempt: number) => Response | Promise<Response>, renewed = true) {
  const requests: Record<string, unknown>[] = [];
  const fetch = vi.fn<typeof globalThis.fetch>(async (_url, init) => {
    const [item] = JSON.parse(init?.body as string) as Record<string, unknown>[];
    requests.push(item!);
    return answer(item!["request_id"] as string, requests.length);
  });
  const transport = createTransport({ baseUrl: "https://app.test", fetch });
  const renew = vi.fn(async () => renewed);
  const send = createSend({ transport, bindings: { [operation]: create }, renew });
  return { send, renew, requests, fetch };
}

function completed(requestId: string): Response {
  return Response.json([{ request_id: requestId, value: { id: "row-1" } }]);
}

describe("send", () => {
  const retriable: Reply = { ok: false, retriable: true };

  it("maps a completed reply to ok", async () => {
    const { send } = harness(completed);
    expect(await send(entry(), new AbortController().signal)).toEqual({ ok: true });
  });

  it.each([
    [
      "network failure",
      () => {
        throw new TypeError("Failed to fetch");
      },
    ],
    ["5xx", () => Response.json({ error: { code: "internal_error" } }, { status: 503 })],
    ["unreadable reply", () => new Response("not JSON")],
  ] as const)("retries uncertainty from %s", async (_name, answer) => {
    const { send } = harness(answer);
    expect(await send(entry(), new AbortController().signal)).toEqual(retriable);
  });

  it.each([
    ["retry", undefined, retriable],
    ["timeout", undefined, retriable],
    [
      "internal_error",
      undefined,
      { ok: false, retriable: false, refusal: { code: "internal_error", text: "internal_error" } },
    ],
    [
      "unique_violation",
      { constraint: "row_label" },
      { ok: false, retriable: false, refusal: { code: "unique_violation", text: "This row already exists." } },
    ],
    [
      "check_violation",
      { constraint: "row_label" },
      {
        ok: false,
        retriable: false,
        refusal: { code: "check_violation", text: '{"detail":{"constraint":"row_label"}}' },
      },
    ],
  ] as const)("maps the declared refusal %s", async (code, detail, expected) => {
    const { send } = harness((requestId) => Response.json([{ request_id: requestId, error: { code, detail } }]));
    expect(await send(entry(), new AbortController().signal)).toEqual(expected);
  });

  it("renews once and resends with the same key and a fresh request id", async () => {
    const { send, renew, requests } = harness((requestId, attempt) =>
      attempt === 1 ? new Response(null, { status: 401 }) : completed(requestId),
    );
    const stored = entry();
    const original = structuredClone(stored);
    expect(await send(stored, new AbortController().signal)).toEqual({ ok: true });
    expect(await send(stored, new AbortController().signal)).toEqual({ ok: true });
    expect(renew).toHaveBeenCalledTimes(1);
    expect(requests).toHaveLength(3);
    expect(new Set(requests.map((item) => item["request_id"])).size).toBe(3);
    for (const item of requests) {
      expect(item).toEqual({ label: "Box", idempotency_key: "stored-hash", request_id: expect.any(String) });
    }
    expect(stored).toEqual(original);
  });

  it.each([false, true])("retries after unauthenticated when renewal returns %s", async (renewed) => {
    const { send, renew, requests } = harness(() => new Response(null, { status: 401 }), renewed);
    expect(await send(entry(), new AbortController().signal)).toEqual(retriable);
    expect(renew).toHaveBeenCalledTimes(1);
    expect(requests).toHaveLength(renewed ? 2 : 1);
  });

  it("retries a failed renewal", async () => {
    const { send, renew, requests } = harness(() => new Response(null, { status: 401 }));
    renew.mockRejectedValueOnce(new TypeError("Failed to fetch"));
    expect(await send(entry(), new AbortController().signal)).toEqual(retriable);
    expect(requests).toHaveLength(1);
  });

  it.each(["absent", "toString"])("refuses the unknown operation %s", async (name) => {
    const { send, fetch, renew } = harness(completed);
    expect(await send({ ...entry(), operation: name }, new AbortController().signal)).toEqual({
      ok: false,
      retriable: false,
      refusal: { code: "unknown_operation", text: "unknown_operation" },
    });
    expect(fetch).not.toHaveBeenCalled();
    expect(renew).not.toHaveBeenCalled();
  });

  it("refuses a partial outcome", async () => {
    const transport: Transport = {
      invoke: async () => ({ status: "partiallyCompleted", committedResult: {}, failedOutcome: null }),
    };
    const send = createSend({ transport, bindings: { [operation]: create }, renew: async () => true });
    expect(await send(entry(), new AbortController().signal)).toEqual({
      ok: false,
      retriable: false,
      refusal: { code: "partial", text: "partial" },
    });
  });
});

describe("requestHash", () => {
  it("sorts keys at every depth, including objects inside arrays", async () => {
    const left = await requestHash({ b: [{ z: 1, a: { b: 2, a: 3 } }], a: true });
    expect(left).toMatch(/^[0-9a-f]{64}$/);
    expect(left).toBe(await requestHash({ a: true, b: [{ a: { a: 3, b: 2 }, z: 1 }] }));
    expect(left).not.toBe(await requestHash({ a: true, b: [{ a: { a: 4, b: 2 }, z: 1 }] }));
  });

  it("uses SHA-256 of compact sorted JSON", async () => {
    expect(await requestHash({ b: 2, a: 1 })).toBe("43258cff783fe7036d8a43033f830adfc60ec037382473548ac742b888292777");
  });
});

it("names the outbox database", () => {
  expect(outboxDatabase("wms", "dev", "user-1")).toBe("wamn-outbox:wms:dev:user-1");
});
