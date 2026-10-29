import "fake-indexeddb/auto";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { Loading, flush } from "solid-js";
import type { JsonValue, Outcome, Transport, WireRequest } from "@wamn/web-runtime";
import type { ScreenProps } from "@wamn/platform-ui";
import {
  RECEIVING_LOAD_OPEN_RECEIPT_LINES_ROUTE,
  RECEIVING_RECORD_RECEIPT_ROUTE,
} from "../src/generated/wamn_receiving/receiving.js";
import { LOCATION_LIST_ROUTE } from "../src/generated/wamn_receiving/location.js";
import { ReceivingLoadOpenReceiptLinesTable } from "../src/generated/wamn_receiving/components/index.js";
import { addDecimal, receiptCall, receivedLine, SyncReceipts } from "../src/sync";

vi.mock("@wamn/platform-ui", async (original) => ({
  ...(await original()),
  useSession: () => ({
    userId: `user-${userCount}`,
    offline: () => false,
    settings: { profile: "session", pendingMaxAge: "PT1H", clearOnSignOut: true },
    onSignOut: () => undefined,
  }),
}));

let userCount = 0;

const line = {
  purchase_order_id: "order-1",
  purchase_order_number: "PO-1",
  purchase_order_status: "open",
  supplier_id: "supplier-1",
  purchase_order_row_version: 3,
  line_id: "line-1",
  line_number: 1,
  item_id: "item-1",
  item_number: "ITEM-1",
  ordered_quantity: "10.000",
  received_quantity: "2.000",
  remaining_quantity: "8.000",
  row_version: 7,
};

/** A host that lists one open line, serves an empty change feed, and answers each receipt as told. */
function host(receipt: () => Outcome<JsonValue>) {
  const receipts: WireRequest[] = [];
  const invoke = vi.fn<Transport["invoke"]>(async (request): Promise<Outcome<JsonValue>> => {
    if (request.operation === LOCATION_LIST_ROUTE.operation)
      return { status: "completed", value: { rows: [{ id: "location-1", location_code: "DOCK" }] } };
    if (request.operation === RECEIVING_RECORD_RECEIPT_ROUTE.operation) {
      receipts.push(request);
      return receipt();
    }
    if (request.operation === RECEIVING_LOAD_OPEN_RECEIPT_LINES_ROUTE.operation) {
      const item = request.items[0] as { feed?: { cursor: string } };
      return item.feed
        ? { status: "completed", value: { rows: [], next_cursor: item.feed.cursor, complete: true } }
        : { status: "completed", value: { item: [line], next_cursor: null, cursor: "c1" } };
    }
    throw new Error(`Unexpected operation: ${request.operation}`);
  });
  const transport: Transport = { invoke, probe: async () => true };
  const props: ScreenProps = {
    transport,
    holds: () => true,
    params: {},
    search: {},
    open: vi.fn(),
    close: vi.fn(),
  };
  return { props, receipts, invoke };
}

beforeEach(() => {
  HTMLElement.prototype.scrollIntoView = () => undefined;
  window.matchMedia = ((query: string) => ({
    matches: false,
    media: query,
    addEventListener() {},
    removeEventListener() {},
  })) as unknown as typeof window.matchMedia;
});

afterEach(() => {
  cleanup();
  userCount += 1;
});

describe("the receipt arithmetic", () => {
  it("adds and subtracts numeric text exactly at the larger scale", () => {
    expect(addDecimal("2.000", "1.5")).toBe("3.500");
    expect(addDecimal("8.000", "-1.5")).toBe("6.500");
    expect(addDecimal("0.1", "0.2")).toBe("0.3");
    expect(addDecimal("1", "-1.25")).toBe("-0.25");
    expect(addDecimal("12", "3")).toBe("15");
  });

  it("shows the receipt's line as tentative with the quantity moved", () => {
    const row = {
      ...line,
      lineId: "line-1",
      receivedQuantity: "2.000",
      remainingQuantity: "8.000",
    } as unknown as Parameters<typeof receivedLine>[0];
    expect(receivedLine(row, "3")).toEqual({
      lineId: "line-1",
      receivedQuantity: "5.000",
      remainingQuantity: "5.000",
      $tentative: true,
    });
  });

  it("keys one receipt call to its line and leaves the key to the send", async () => {
    const request = {
      value: {
        purchaseOrderId: "order-1",
        receiptReference: "R-1",
        occurredAt: "2026-10-28T00:00:00.000000Z",
        line: [{ purchaseOrderLineId: "line-1", locationId: "location-1", quantity: "3" }],
      },
    };
    const call = await receiptCall(request);
    expect(call).toEqual({
      operation: RECEIVING_RECORD_RECEIPT_ROUTE.operation,
      request,
      request_hash: expect.stringMatching(/^[0-9a-f]{64}$/),
      expect: { model: "purchase_order_line", id: "line-1" },
      append_only: false,
    });
  });
});

describe("the open receipt lines screen", () => {
  history.replaceState(null, "", "/urn:wamn:project-env:acme:wamn:dev:one/open-receipt-lines");

  async function record(quantity: string) {
    const table = await screen.findByRole("table", { name: ReceivingLoadOpenReceiptLinesTable.title });
    fireEvent.click(await within(table).findByRole("button", { name: "Record a receipt" }, { timeout: 5000 }));
    const form = await screen.findByRole("form", { name: "Record a receipt" });
    fireEvent.input(within(form).getByLabelText("Receipt reference"), { target: { value: "R-1" } });
    fireEvent.input(within(form).getByLabelText("Quantity"), { target: { value: quantity } });
    await within(form).findByRole("option", { name: "DOCK" });
    fireEvent.change(within(form).getByLabelText("Location"), { target: { value: "location-1" } });
    flush();
    fireEvent.submit(form);
    return table;
  }

  it("shows a recorded receipt as tentative and sends its key inside value", async () => {
    let answer: Outcome<JsonValue> = { status: "uncertain", reason: "offline", retryRefusal: null };
    const h = host(() => answer);
    render(() => (
      <Loading>
        <SyncReceipts screen={h.props} description={ReceivingLoadOpenReceiptLinesTable} />
      </Loading>
    ));
    const table = await record("3");
    await waitFor(() => expect(table.querySelector("[data-tentative]")).not.toBeNull());
    await waitFor(() => expect(within(table).getAllByText("5.000").length).toBe(2));
    await waitFor(() => expect(screen.getByLabelText("Sync status").textContent).toContain("Pending: 1"));
    // The panel closes once the outbox holds the receipt, before any host answer.
    await waitFor(() => expect(screen.queryByRole("form", { name: "Record a receipt" })).toBeNull());
    await waitFor(() => expect(h.receipts.length).toBeGreaterThan(0));
    const sent = h.receipts[0]!.items[0] as { request_id: string; value: Record<string, unknown> };
    expect(sent.value.idempotency_key).toMatch(/^[0-9a-f]{64}$/);
    expect(sent.value.line).toEqual([{ purchase_order_line_id: "line-1", location_id: "location-1", quantity: "3" }]);
    expect(Object.keys(sent).sort()).toEqual(["request_id", "value"]);
    answer = {
      status: "completed",
      value: { receipt_id: "r-1", purchase_order_id: "order-1", purchase_order_status: "open", row_version: 4 },
    };
  });

  it("puts a refused receipt in the not uploaded panel and resubmits a correction under the same line", async () => {
    let refuse = true;
    const h = host(() =>
      refuse
        ? { status: "refused", code: "quantity_exceeds_remaining", text: "Too many.", detail: null }
        : {
            status: "completed",
            value: { receipt_id: "r-1", purchase_order_id: "order-1", purchase_order_status: "open", row_version: 4 },
          },
    );
    render(() => (
      <Loading>
        <SyncReceipts screen={h.props} description={ReceivingLoadOpenReceiptLinesTable} />
      </Loading>
    ));
    await record("30");
    const panel = await screen.findByRole("region", { name: "Not uploaded" });
    await within(panel).findByText("Too many.");
    expect(within(panel).getByText(/Record id: line-1/)).toBeDefined();
    refuse = false;
    const corrected = (within(panel).getByLabelText("Corrected request") as HTMLTextAreaElement).value.replace(
      '"quantity": "30"',
      '"quantity": "3"',
    );
    fireEvent.input(within(panel).getByLabelText("Corrected request"), { target: { value: corrected } });
    flush();
    fireEvent.click(within(panel).getByRole("button", { name: "Resubmit" }));
    await waitFor(() => expect(h.receipts.length).toBe(2));
    expect((h.receipts[1]!.items[0] as { value: { line: { quantity: string }[] } }).value.line[0]!.quantity).toBe("3");
    await waitFor(() => expect(within(panel).queryByText("Too many.")).toBeNull());
  });
});
