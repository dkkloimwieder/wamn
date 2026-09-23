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

/** One transport that answers the read with a record and the removal with an empty result. */
export function deleteStub(): { transport: Transport; sent: WireRequest[] } {
  const sent: WireRequest[] = [];
  return {
    sent,
    transport: {
      invoke: (request: WireRequest) => {
        sent.push(request);
        const reply: Outcome<JsonValue> = request.operation.includes("/get@")
          ? {
              status: "completed",
              value: {
                id: WIDGET,
                code: "standard",
                note: null,
                edit_version: "7",
                created_at: "2026-09-21T12:00:00.000000Z",
              },
            }
          : { status: "completed", value: {} };
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
