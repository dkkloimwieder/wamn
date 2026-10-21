import { cleanup, fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { flush } from "solid-js";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { JsonValue, Transport } from "@wamn/web-runtime";
import type { ScreenProps } from "@wamn/platform-ui";
import { RECEIVING_RECORD_RECEIPT_ROUTE } from "@wamn/receiving-client/receiving.js";
import {
  PURCHASE_ORDER_GET_ROUTE,
  PURCHASE_ORDER_QUERY_ROUTE,
  PURCHASE_ORDER_UPDATE_ROUTE,
} from "@wamn/receiving-client/purchase_order.js";
import { SUPPLIER_CREATE_ROUTE, SUPPLIER_QUERY_ROUTE } from "@wamn/receiving-client/supplier.js";
import {
  PurchaseOrderQueryTable,
  PurchaseOrderUpdateForm,
  SupplierCreateForm,
} from "@wamn/receiving-client/components/index.js";
import { Records } from "../src/table";
import { PurchaseOrderForm, ReceiptForm, SupplierForm } from "../src/forms";

beforeEach(() => {
  HTMLElement.prototype.scrollIntoView = () => undefined;
  window.matchMedia = ((query: string) => ({
    matches: false,
    media: query,
    addEventListener() {},
    removeEventListener() {},
  })) as unknown as typeof window.matchMedia;
});
afterEach(cleanup);

function props(invoke: Transport["invoke"]): ScreenProps {
  return {
    transport: { invoke },
    holds: () => true,
    params: { aud: "receiving" },
    search: {},
    open: vi.fn(),
    close: vi.fn(),
  };
}

describe("Receiving descriptions and bindings", () => {
  it("shows and selects a supplier from the query page in the emitted purchase order form", async () => {
    const supplierId = "00000000-0000-0000-0000-000000000401";
    const orderId = "00000000-0000-0000-0000-000000000501";
    const invoke = vi.fn<Transport["invoke"]>(async (request): ReturnType<Transport["invoke"]> => {
      switch (request.operation) {
        case PURCHASE_ORDER_GET_ROUTE.operation:
          return { status: "completed", value: { id: orderId, supplier_id: supplierId, row_version: 1 } };
        case SUPPLIER_QUERY_ROUTE.operation:
          return {
            status: "completed",
            value: {
              item: [{ id: supplierId, name: "SUPPLIER-BROWSER", created_at: "2026-10-21T00:00:00Z" }],
              next_cursor: null,
            },
          };
        case PURCHASE_ORDER_UPDATE_ROUTE.operation:
          return { status: "completed", value: { id: orderId, supplier_id: supplierId, row_version: 2 } };
        default:
          throw new Error(`Unexpected operation: ${request.operation}`);
      }
    });
    const view = { ...props(invoke), params: { id: orderId } };
    render(() => <PurchaseOrderForm {...view} />);
    const input = await screen.findByRole<HTMLInputElement>("combobox", {
      name: PurchaseOrderUpdateForm.fields[0].label,
    });
    await waitFor(() => expect(input.value).toBe("SUPPLIER-BROWSER"));
    fireEvent.focus(input);
    fireEvent.click(input);
    fireEvent.click(await screen.findByRole("option", { name: "SUPPLIER-BROWSER" }));
    flush();
    expect(input.value).toBe("SUPPLIER-BROWSER");
    fireEvent.submit(screen.getByRole("button", { name: PurchaseOrderUpdateForm.submit.label }).closest("form")!);
    await waitFor(() => expect(view.open).toHaveBeenCalledWith("purchase-orders"));
    expect(invoke).toHaveBeenLastCalledWith({
      ...PURCHASE_ORDER_UPDATE_ROUTE,
      items: [
        { request_id: expect.any(String), id: orderId, expected_row_version: 1, change: { supplier_id: supplierId } },
      ],
    });
  });

  it("puts route actions in the header, follows record links, and pages server cursors without losing prior rows", async () => {
    const first = Array.from({ length: 25 }, (_, index) => ({
      id: String(index + 1),
      purchase_order_number: `PO-${index + 1}`,
    }));
    const invoke = vi
      .fn<Transport["invoke"]>()
      .mockResolvedValueOnce({ status: "completed", value: { rows: first, next_cursor: "page-two" } })
      .mockResolvedValueOnce({
        status: "completed",
        value: { rows: [{ id: "26", purchase_order_number: "PO-26" }], next_cursor: null },
      });
    const view = props(invoke);
    render(() => (
      <Records
        screen={{
          ...view,
          actions: [{ label: SupplierCreateForm.title, path: "suppliers/new", operation: SupplierCreateForm.id }],
        }}
        description={PurchaseOrderQueryTable}
        route={PURCHASE_ORDER_QUERY_ROUTE}
        rowActions={[{ description: { id: "get", title: "Open order" }, path: (row) => `purchase-orders/${row?.id}` }]}
      />
    ));
    const table = await screen.findByRole("table", { name: PurchaseOrderQueryTable.title });
    await within(table).findByText("PO-1");
    expect(
      screen.getByRole("link", { name: SupplierCreateForm.title }).closest(".table-workspace-header"),
    ).not.toBeNull();
    const open = within(table).getAllByRole("link", { name: "Open order" })[0]!;
    expect(open.getAttribute("href")).toBe("/receiving/purchase-orders/1");
    fireEvent.click(open);
    expect(view.open).toHaveBeenCalledWith("purchase-orders/1");
    fireEvent.click(screen.getByRole("button", { name: "Next page" }));
    await within(table).findByText("PO-26");
    expect(invoke.mock.calls[1]![0].items).toEqual([{ limit: 25, cursor: "page-two" }]);
    expect(within(table).queryByText("PO-1")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Previous page" }));
    flush();
    expect(within(table).getByText("PO-1")).toBeDefined();
    expect(invoke).toHaveBeenCalledTimes(2);
  });

  it("keeps accepted rows when the next page fails and retries the same cursor", async () => {
    const invoke = vi
      .fn<Transport["invoke"]>()
      .mockResolvedValueOnce({
        status: "completed",
        value: { rows: [{ id: "1", purchase_order_number: "PO-1" }], next_cursor: "retry" },
      })
      .mockRejectedValueOnce(new Error("Disconnected"))
      .mockResolvedValueOnce({ status: "completed", value: { rows: [], next_cursor: null } });
    render(() => (
      <Records screen={props(invoke)} description={PurchaseOrderQueryTable} route={PURCHASE_ORDER_QUERY_ROUTE} />
    ));
    await screen.findByText("PO-1");
    fireEvent.click(screen.getByRole("button", { name: "Next page" }));
    await screen.findByText(/Disconnected/);
    expect(screen.getByText("PO-1")).toBeDefined();
    fireEvent.click(screen.getByRole("button", { name: "Next page" }));
    await waitFor(() => expect(invoke).toHaveBeenCalledTimes(3));
    expect(invoke.mock.calls[2]![0].items).toEqual([{ limit: 25, cursor: "retry" }]);
  });

  it("validates the described supplier form and returns to its table only after the write", async () => {
    const invoke = vi.fn<Transport["invoke"]>(async (request) => {
      const input = request.items[0] as Record<string, JsonValue>;
      if (!input.idempotency_key)
        return { status: "refused", code: "invalid_input", detail: { field: "idempotency_key" } };
      return { status: "completed", value: { id: "new", name: input.name! } };
    });
    const view = props(invoke);
    render(() => <SupplierForm {...view} />);
    fireEvent.submit(screen.getByRole("button", { name: SupplierCreateForm.title }).closest("form")!);
    await screen.findByText("Enter a value.");
    expect(invoke).not.toHaveBeenCalled();
    fireEvent.input(screen.getByLabelText("Supplier name"), { target: { value: "BROWSER-NEW-SUPPLIER" } });
    flush();
    fireEvent.submit(screen.getByRole("button", { name: SupplierCreateForm.title }).closest("form")!);
    await waitFor(() => expect(view.open).toHaveBeenCalledWith("suppliers"));
    expect(invoke).toHaveBeenCalledWith({
      ...SUPPLIER_CREATE_ROUTE,
      items: [{ name: "BROWSER-NEW-SUPPLIER", request_id: expect.any(String), idempotency_key: expect.any(String) }],
    });
  });
  it("keeps the supplier key on retry and replaces it when the name changes", async () => {
    const invoke = vi
      .fn<Transport["invoke"]>()
      .mockRejectedValueOnce(new Error("Disconnected"))
      .mockRejectedValueOnce(new Error("Disconnected"))
      .mockResolvedValueOnce({ status: "completed", value: { id: "new" } });
    const view = props(invoke);
    render(() => <SupplierForm {...view} />);
    const submit = () => fireEvent.submit(screen.getByRole("button", { name: "Add a supplier" }).closest("form")!);
    fireEvent.input(screen.getByLabelText("Supplier name"), { target: { value: "New supplier" } });
    flush();
    submit();
    await screen.findByText("Disconnected");
    expect(view.open).not.toHaveBeenCalled();
    submit();
    await waitFor(() => expect(invoke).toHaveBeenCalledTimes(2));
    await screen.findByText("Disconnected");
    const writes = () => invoke.mock.calls.map(([request]) => request.items[0] as Record<string, JsonValue>);
    expect(writes()[0]!.idempotency_key).toEqual(expect.any(String));
    expect(writes()[1]!.idempotency_key).toBe(writes()[0]!.idempotency_key);
    expect(writes()[1]!.request_id).not.toBe(writes()[0]!.request_id);
    fireEvent.input(screen.getByLabelText("Supplier name"), { target: { value: "Changed supplier" } });
    flush();
    submit();
    await waitFor(() => expect(view.open).toHaveBeenCalledWith("suppliers"));
    expect(writes()[2]!.idempotency_key).not.toBe(writes()[0]!.idempotency_key);
    expect(writes()[2]!.name).toBe("Changed supplier");
  });
  it("keeps the receipt intent on retry and replaces it when the operator changes the draft", async () => {
    const writes: JsonValue[] = [];
    const invoke: Transport["invoke"] = async (request) => {
      if (request.operation !== RECEIVING_RECORD_RECEIPT_ROUTE.operation)
        return { status: "completed", value: { rows: [] } };
      writes.push(request.items[0]!);
      if (writes.length < 3) throw new Error("Try again");
      return { status: "completed", value: null };
    };
    const view = props(invoke);
    render(() => (
      <ReceiptForm
        {...view}
        search={{
          "value.purchaseOrderId": "order",
          "value.receiptReference": "receipt",
          "value.line.0.locationId": "dock",
          "value.line.0.purchaseOrderLineId": "line",
          "value.line.0.quantity": "1",
        }}
      />
    ));
    const submit = () => fireEvent.submit(screen.getByRole("button", { name: "Record a receipt" }).closest("form")!);
    submit();
    await screen.findByText("Try again");
    submit();
    await waitFor(() => expect(writes).toHaveLength(2));
    await screen.findByText("Try again");
    const values = () => writes.map((write) => (write as { value: Record<string, JsonValue> }).value);
    expect(values()[0]).toEqual(values()[1]);
    expect(values()[0]!.line).toEqual([{ location_id: "dock", purchase_order_line_id: "line", quantity: "1" }]);
    fireEvent.input(screen.getByLabelText("Receipt reference"), { target: { value: "changed" } });
    flush();
    submit();
    await waitFor(() => expect(view.open).toHaveBeenCalledWith("receipts"));
    expect(values()[2]!.idempotency_key).not.toBe(values()[0]!.idempotency_key);
    expect(values()[2]!.receipt_reference).toBe("changed");
  });
});
