/**
 * The stub transports and the sample rows that the component tests and the
 * gallery share.
 *
 * Each stub answers the fixture's operations with sample data and makes no
 * request. The tests read what a stub was sent, and the gallery renders what
 * it answers, so the sample data has one owner.
 */

import type { JsonValue, Outcome, Transport, WireRequest } from "@wamn/web-runtime";

/** The key of the one maker that every selector offers first. */
export const MAKER = "0f1e2d3c-4b5a-4968-8778-695a4b3c2d1e";

/** The key of the maker that a search or a second page finds. */
export const SOUTH = "1a2b3c4d-5e6f-4a7b-8c9d-0e1f2a3b4c5d";

/** The key of the one widget that a detail, a group and a removal read. */
export const WIDGET = "0f1e2d3c-4b5a-4968-8778-695a4b3c2d1e";

/** A transport that answers every request with one outcome. */
export function sampleStub(outcome: Outcome<JsonValue>): Transport {
  return {
    invoke: (_request: WireRequest) => Promise.resolve(outcome),
  };
}

/** One transport that answers from a list and keeps what it was sent. */
export function tableStub(replies: readonly Outcome<JsonValue>[]): {
  transport: Transport;
  sent: WireRequest[];
} {
  const sent: WireRequest[] = [];
  let next = 0;
  return {
    sent,
    transport: {
      invoke: (request: WireRequest) => {
        sent.push(request);
        const reply = replies[Math.min(next, replies.length - 1)];
        next += 1;
        return Promise.resolve(
          reply ?? { status: "uncertain", reason: "the stub ran out", retryRefusal: null },
        );
      },
    },
  };
}

/** One page of widgets with the named keys, and the cursor of the next page. */
export function page(ids: readonly string[], cursor: string | null): Outcome<JsonValue> {
  return {
    status: "completed",
    value: {
      item: ids.map((id) => ({
        id,
        code: "standard",
        note: null,
        edit_version: "1",
        created_at: "2026-09-21T12:00:00.000000Z",
      })),
      next_cursor: cursor,
    },
  };
}

/** The key of a maker that no read finds. */
export const GONE = "9e8d7c6b-5a49-4382-9170-6f5e4d3c2b1a";

/**
 * One transport whose widgets name makers, and which reads one maker by key.
 *
 * Four widgets name three makers, and one names none, so a table that reads
 * each maker once sends three maker reads. `GONE` answers not found.
 */
export function makerStub(): { transport: Transport; sent: WireRequest[] } {
  const sent: WireRequest[] = [];
  const makers: { readonly [key: string]: string } = { [MAKER]: "Northwind", [SOUTH]: "Southwind" };
  const widget = (id: string, maker: string | null) => ({
    id,
    code: "standard",
    note: null,
    maker_id: maker,
    edit_version: "1",
    created_at: "2026-09-21T12:00:00.000000Z",
  });
  return {
    sent,
    transport: {
      invoke: (request: WireRequest) => {
        sent.push(request);
        if (!request.operation.includes("widget-maker")) {
          return Promise.resolve<Outcome<JsonValue>>({
            status: "completed",
            value: {
              item: [
                widget("a", MAKER),
                widget("b", SOUTH),
                widget("c", MAKER),
                widget("d", GONE),
                widget("e", null),
              ],
              next_cursor: null,
            },
          });
        }
        const key = String((request.items[0] as { id?: string }).id);
        const name = makers[key];
        return Promise.resolve<Outcome<JsonValue>>(
          name === undefined
            ? { status: "refused", code: "not_found", detail: null }
            : { status: "completed", value: { id: key, name, created_at: "2026-09-21T12:00:00.000000Z" } },
        );
      },
    },
  };
}

/** One transport that answers each operation from its own reply. */
export function selectorStub(): { transport: Transport; sent: WireRequest[] } {
  const sent: WireRequest[] = [];
  return {
    sent,
    transport: {
      invoke: (request: WireRequest) => {
        sent.push(request);
        const reply: Outcome<JsonValue> = request.operation.includes("widget-maker")
          ? {
              status: "completed",
              value: { item: [{ id: MAKER, name: "Northwind" }], nextCursor: null },
            }
          : { status: "completed", value: { id: "written", edit_version: 1 } };
        return Promise.resolve(reply);
      },
    },
  };
}

/** One transport that answers a search and a next page from the same list. */
export function paged(): { transport: Transport; sent: WireRequest[] } {
  const sent: WireRequest[] = [];
  return {
    sent,
    transport: {
      invoke: (request: WireRequest) => {
        sent.push(request);
        if (!request.operation.includes("widget-maker")) {
          return Promise.resolve<Outcome<JsonValue>>({
            status: "completed",
            value: { id: "written", edit_version: 1 },
          });
        }
        const item = request.items[0] as {
          filter?: { name?: string[] };
          cursor?: string;
        };
        if (item.filter?.name !== undefined) {
          return Promise.resolve<Outcome<JsonValue>>({
            status: "completed",
            value: { item: [{ id: SOUTH, name: "Southwind" }], nextCursor: null },
          });
        }
        if (item.cursor === "page-2") {
          return Promise.resolve<Outcome<JsonValue>>({
            status: "completed",
            value: { item: [{ id: SOUTH, name: "Southwind" }], nextCursor: null },
          });
        }
        return Promise.resolve<Outcome<JsonValue>>({
          status: "completed",
          value: { item: [{ id: MAKER, name: "Northwind" }], nextCursor: "page-2" },
        });
      },
    },
  };
}

/** One transport for a repeated group: a maker page and a bounded line list. */
export function groupStub(): { transport: Transport; sent: WireRequest[] } {
  const sent: WireRequest[] = [];
  return {
    sent,
    transport: {
      invoke: (request: WireRequest) => {
        sent.push(request);
        // The maker selector reads a paged query and the line selector reads
        // a bounded list, so each envelope answers the operation that asked.
        const row = { id: WIDGET, code: "priority", name: "Northwind" };
        const reply: Outcome<JsonValue> = request.operation.includes("widget-maker")
          ? { status: "completed", value: { item: [row], nextCursor: null } }
          : { status: "completed", value: { rows: [row] } };
        return Promise.resolve(reply);
      },
    },
  };
}

/**
 * One transport that holds one widget at revision 7, as a server does.
 *
 * A removal with that revision completes with an empty result, and a removal
 * with any other refuses as a conflict. `write` is a second writer that moves
 * the revision after the page displayed the record.
 */
export function deleteStub(): { transport: Transport; sent: WireRequest[]; write: () => void } {
  const sent: WireRequest[] = [];
  let version = 7;
  return {
    sent,
    write: () => {
      version += 1;
    },
    transport: {
      invoke: (request: WireRequest) => {
        sent.push(request);
        const item = request.items[0] as { [key: string]: JsonValue };
        const expected = item["expected_edit_version"];
        const reply: Outcome<JsonValue> =
          expected === String(version)
            ? { status: "completed", value: {} }
            : {
                status: "refused",
                code: "concurrency_conflict",
                detail: { expected: expected ?? null, observed: String(version) },
              };
        return Promise.resolve(reply);
      },
    },
  };
}

/** One transport that offers one maker and answers every other read with no row. */
export function prefillStub(): Transport {
  return {
    invoke: (request: WireRequest) => {
      const reply: Outcome<JsonValue> = request.operation.includes("widget-maker")
        ? {
            status: "completed",
            value: { item: [{ id: MAKER, name: "Northwind" }], nextCursor: null },
          }
        : { status: "completed", value: { rows: [] } };
      return Promise.resolve(reply);
    },
  };
}

/**
 * One transport that holds one widget and its revision, as a server does.
 *
 * A read answers the current revision. An update with that revision completes
 * and moves it, and an update with any other refuses as a conflict. `write`
 * is a second writer that moves the revision behind the form's back. An
 * update to the code `taken` refuses as a unique violation.
 */
export function updateStub(
  taken: string | null = null,
): { transport: Transport; sent: WireRequest[]; write: () => void } {
  const sent: WireRequest[] = [];
  let version = 7;
  const row = (): JsonValue => ({
    id: WIDGET,
    code: "standard",
    note: null,
    edit_version: String(version),
    created_at: "2026-09-21T12:00:00.000000Z",
  });
  return {
    sent,
    write: () => {
      version += 1;
    },
    transport: {
      invoke: (request: WireRequest) => {
        sent.push(request);
        if (request.operation.includes("/get@")) {
          return Promise.resolve({ status: "completed", value: row() });
        }
        if (request.operation.includes("widget-maker")) {
          return Promise.resolve({ status: "completed", value: { item: [], nextCursor: null } });
        }
        const item = request.items[0] as { [key: string]: JsonValue };
        const expected = item["expected_edit_version"];
        if (expected !== String(version)) {
          return Promise.resolve({
            status: "refused",
            code: "concurrency_conflict",
            detail: { expected: expected ?? null, observed: String(version) },
          });
        }
        // Another widget holds `taken`, so the unique key refuses it. The
        // generated codec names the field that the constraint guards.
        const change = item["change"] as { [key: string]: JsonValue } | undefined;
        if (taken !== null && change?.["code"] === taken) {
          return Promise.resolve({
            status: "refused",
            code: "unique_violation",
            detail: { constraint: "widget_code_key", field: "change.code" },
          });
        }
        version += 1;
        return Promise.resolve({ status: "completed", value: row() });
      },
    },
  };
}
