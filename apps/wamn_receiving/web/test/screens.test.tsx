import { cleanup, fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { flush } from "solid-js";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { JsonValue, Transport } from "@wamn/web-runtime";
import type { ScreenProps } from "@wamn/platform-ui";
import {
  RECEIVING_LOAD_PURCHASE_ORDER_HISTORY_ROUTE,
  RECEIVING_LOAD_RECEIPT_SCREEN_ROUTE,
  RECEIVING_RECORD_RECEIPT_ROUTE,
} from "../src/generated/wamn_receiving/receiving.js";
import {
  PURCHASE_ORDER_GET_ROUTE,
  PURCHASE_ORDER_QUERY_ROUTE,
  PURCHASE_ORDER_UPDATE_ROUTE,
} from "../src/generated/wamn_receiving/purchase_order.js";
import { SUPPLIER_CREATE_ROUTE, SUPPLIER_QUERY_ROUTE } from "../src/generated/wamn_receiving/supplier.js";
import {
  PurchaseOrderQueryTable,
  PurchaseOrderGetDetail,
  ReceiptGetDetail,
  SupplierQueryTable,
  LocationListTable,
  PurchaseOrderUpdateForm,
  SupplierCreateForm,
} from "../src/generated/wamn_receiving/components/index.js";
import { Records } from "../src/table";
import { SECTIONS } from "../src/routes";
import { Selector } from "../src/selector";
import {
  ReceiptQueryTable,
  ReceivingLoadPurchaseOrderHistoryTable,
  ReceivingLoadReceiptScreenTable,
  ReceivingRecordReceiptForm,
} from "../src/generated/wamn_receiving/components/index.js";
import { RECEIPT_GET_ROUTE, RECEIPT_QUERY_ROUTE } from "../src/generated/wamn_receiving/receipt.js";
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

function expectFormActions(submitLabel: string) {
  const submit = screen.getByRole("button", { name: submitLabel });
  const form = submit.closest("form")!;
  const rows = form.querySelectorAll(":scope > .ui-form-actions");
  expect(rows).toHaveLength(1);
  expect(within(rows[0] as HTMLElement).getAllByRole("button")).toEqual([
    submit,
    within(form).getByRole("button", { name: "Cancel" }),
  ]);
  expect(submit.getAttribute("type")).toBe("submit");
  expect(within(form).getByRole("button", { name: "Cancel" }).getAttribute("type")).toBe("button");
  expect(form.lastElementChild).toBe(rows[0]);
}

describe("Receiving descriptions and bindings", () => {
  it("shows each filterable heading once and keeps its input accessible", async () => {
    const invoke = vi.fn<Transport["invoke"]>(async () => ({
      status: "completed",
      value: { rows: [] },
    }));
    const Table = SECTIONS[0]!.screens[0]!.component;
    render(() => <Table {...props(invoke)} />);
    const table = await screen.findByRole("table", { name: "Purchase orders" });
    for (const label of ["Order number", "Status", "Supplier"]) {
      const input = within(table).getByRole("textbox", { name: label });
      const header = input.closest("th")!;
      expect(within(header).getAllByText(label, { exact: true, selector: ":not(.column-move)" })).toHaveLength(1);
      expect(header.querySelector(".column-heading .column-sort-label")?.textContent).toBe(label);
    }
    fireEvent.input(within(table).getByRole("textbox", { name: "Order number" }), {
      target: { value: "PO-FILTER" },
    });
    flush();
    fireEvent.click(screen.getByRole("button", { name: "Refresh results" }));
    await waitFor(() =>
      expect(invoke).toHaveBeenLastCalledWith(
        expect.objectContaining({ items: [{ limit: 25, filter: { purchase_order_number: ["PO-FILTER"] } }] }),
      ),
    );
  });

  it("never sorts or filters the Actions column", async () => {
    const invoke: Transport["invoke"] = async () => ({ status: "completed", value: { rows: [] } });
    render(() => (
      <Records
        screen={props(invoke)}
        description={PurchaseOrderQueryTable}
        route={PURCHASE_ORDER_QUERY_ROUTE}
        filters={["actions"]}
        rowActions={[{ description: { id: "action", title: "Row action" }, path: () => "action" }]}
      />
    ));
    const table = await screen.findByRole("table", { name: "Purchase orders" });
    const header = table.querySelector<HTMLElement>('th[data-column-header="actions"]')!;
    expect(within(header).queryByRole("textbox")).toBeNull();
    expect(within(header).queryByRole("button", { name: "Sort by Actions" })).toBeNull();
    expect(within(table).getByRole<HTMLButtonElement>("button", { name: "Sort by Order number" }).disabled).toBe(false);
  });

  it("selects human fields from the generated descriptions", () => {
    expect(Object.keys(PurchaseOrderQueryTable.columns).sort()).toEqual([
      "purchase_order_number",
      "row_version",
      "status",
      "supplier_id",
    ]);
    expect(PurchaseOrderGetDetail.fields.map((field) => field.field).sort()).toEqual(
      Object.keys(PurchaseOrderQueryTable.columns).sort(),
    );
    expect(Object.keys(ReceiptQueryTable.columns).sort()).toEqual([
      "occurred_at",
      "purchase_order_id",
      "receipt_reference",
    ]);
    expect(ReceiptGetDetail.fields.map((field) => field.field).sort()).toEqual(
      Object.keys(ReceiptQueryTable.columns).sort(),
    );
    expect(Object.keys(SupplierQueryTable.columns)).toEqual(["name"]);
    expect(Object.keys(LocationListTable.columns)).toEqual(["location_code"]);
  });

  it("shows supplier names in order rows and details, with label links and receipt buttons", async () => {
    const supplierId = "00000000-0000-0000-0000-000000000401";
    const order = { id: "order-id", purchase_order_number: "PO-HUMAN", supplier_id: supplierId };
    const invoke = vi.fn<Transport["invoke"]>(async (request): ReturnType<Transport["invoke"]> => {
      if (request.operation === PURCHASE_ORDER_QUERY_ROUTE.operation)
        return {
          status: "completed",
          value: { rows: [order, { ...order, id: "order-two", purchase_order_number: "PO-TWO" }] },
        };
      if (request.operation === PURCHASE_ORDER_GET_ROUTE.operation) return { status: "completed", value: order };
      if (request.operation === SUPPLIER_QUERY_ROUTE.operation) {
        const input = request.items[0] as Record<string, JsonValue>;
        return {
          status: "completed",
          value: input.cursor
            ? { rows: [{ id: supplierId, name: "Human supplier" }], next_cursor: null }
            : { rows: [{ id: "another", name: "Another supplier" }], next_cursor: "suppliers-two" },
        };
      }
      throw new Error(`Unexpected operation: ${request.operation}`);
    });
    const view = { ...props(invoke), params: { id: order.id, aud: "receiving" } };
    const Table = SECTIONS[0]!.screens[0]!.component;
    render(() => <Table {...view} />);
    const link = await screen.findByRole("link", { name: "PO-HUMAN" });
    const row = link.closest("tr")!;
    expect(within(row).getByText("Human supplier")).toBeDefined();
    expect(row.textContent).not.toContain(supplierId);
    expect(screen.getAllByRole("columnheader")).toHaveLength(5);
    for (const label of ["Order number", "Supplier", "Status", "Revision"])
      expect(screen.getByRole("columnheader", { name: new RegExp(`^${label} `) })).toBeDefined();
    expect(screen.queryByRole("columnheader", { name: /^Purchase order Drag/ })).toBeNull();
    expect(screen.queryByRole("button", { name: "Change the supplier" })).toBeNull();
    expect(screen.getAllByRole("columnheader", { name: /^Actions / })).toHaveLength(1);
    fireEvent.click(within(row).getByRole("button", { name: "Record a receipt" }));
    expect(view.open).toHaveBeenLastCalledWith(expect.stringContaining("receipts/new?"));
    expect(view.open).toHaveBeenLastCalledWith(expect.stringContaining("order-id"));
    expect(invoke.mock.calls.filter(([request]) => request.operation === SUPPLIER_QUERY_ROUTE.operation)).toHaveLength(
      2,
    );
    cleanup();
    const Detail = SECTIONS[0]!.routes!.find((route) => route.path === "purchase-orders/:id")!.component;
    render(() => <Detail {...view} />);
    await screen.findByText("Human supplier");
    expect(document.body.textContent).not.toContain(supplierId);
    expect(
      Array.from(document.querySelectorAll("[data-generator-field]"), (node) =>
        node.getAttribute("data-generator-field"),
      ).sort(),
    ).toEqual(["purchase_order_number", "row_version", "status", "supplier_id"]);
    fireEvent.click(screen.getByRole("button", { name: "Record a receipt" }));
    expect(view.open).toHaveBeenLastCalledWith(expect.stringContaining("order-id"));
    fireEvent.click(screen.getByRole("button", { name: "Change the supplier" }));
    expect(view.open).toHaveBeenLastCalledWith("purchase-orders/order-id/update");
    expect(screen.getByRole("link", { name: "Receiving screen" })).toBeDefined();
  });

  it("shows the purchase order number in receipt rows and details and links the receipt label", async () => {
    const orderId = "00000000-0000-0000-0000-000000000501";
    const receipt = { id: "receipt-id", receipt_reference: "RECEIPT-HUMAN", purchase_order_id: orderId };
    const invoke = vi.fn<Transport["invoke"]>(async (request): ReturnType<Transport["invoke"]> => {
      if (request.operation === RECEIPT_QUERY_ROUTE.operation)
        return { status: "completed", value: { rows: [receipt] } };
      if (request.operation === RECEIPT_GET_ROUTE.operation) return { status: "completed", value: receipt };
      if (request.operation === PURCHASE_ORDER_GET_ROUTE.operation)
        return { status: "completed", value: { id: orderId, purchase_order_number: "PO-HUMAN" } };
      if (request.operation === PURCHASE_ORDER_QUERY_ROUTE.operation)
        return { status: "completed", value: { rows: [{ id: orderId, purchase_order_number: "PO-HUMAN" }] } };
      throw new Error(`Unexpected operation: ${request.operation}`);
    });
    const view = { ...props(invoke), params: { id: receipt.id, aud: "receiving" } };
    const Table = SECTIONS[1]!.screens[0]!.component;
    render(() => <Table {...view} />);
    const table = await screen.findByRole("table", { name: ReceiptQueryTable.title });
    const link = await within(table).findByRole("link", { name: "RECEIPT-HUMAN" });
    expect(within(table).getByText("PO-HUMAN")).toBeDefined();
    expect(table.textContent).not.toContain(orderId);
    expect(within(table).queryByRole("columnheader", { name: /^Receipt Drag/ })).toBeNull();
    fireEvent.click(link);
    expect(view.open).toHaveBeenLastCalledWith("receipts/receipt-id");
    cleanup();
    const Detail = SECTIONS[1]!.routes!.find((route) => route.path === "receipts/:id")!.component;
    render(() => <Detail {...view} />);
    await screen.findByText("PO-HUMAN");
    expect(document.body.textContent).not.toContain(orderId);
  });

  it("keeps multiple row operations in one action cell and honors operation grants", async () => {
    const invoke: Transport["invoke"] = async () => ({
      status: "completed",
      value: { rows: [{ id: "order", purchase_order_number: "PO-1" }] },
    });
    const view = { ...props(invoke), holds: (operation: string) => operation !== "hidden" };
    render(() => (
      <Records
        screen={view}
        description={PurchaseOrderQueryTable}
        route={PURCHASE_ORDER_QUERY_ROUTE}
        rowActions={[
          { description: { id: "first", title: "First action" }, path: () => "first" },
          { description: { id: "second", title: "Second action" }, path: () => "second" },
          { description: { id: "hidden", title: "Hidden action" }, path: () => "hidden" },
        ]}
      />
    ));
    const first = await screen.findByRole("button", { name: "First action" });
    expect(first.closest("td")).toBe(screen.getByRole("button", { name: "Second action" }).closest("td"));
    expect(screen.getAllByRole("columnheader", { name: /^Actions / })).toHaveLength(1);
    expect(screen.queryByRole("button", { name: "Hidden action" })).toBeNull();
  });

  it("resolves a held selector value beyond the first page without displaying its UUID", async () => {
    const id = "00000000-0000-0000-0000-000000000401";
    let release: (() => void) | undefined;
    const waiting = new Promise<void>((resolve) => {
      release = resolve;
    });
    const invoke: Transport["invoke"] = async (request): ReturnType<Transport["invoke"]> => {
      const input = request.items[0] as Record<string, JsonValue>;
      if (input.cursor) {
        await waiting;
        return { status: "completed", value: { rows: [{ id, name: "Later supplier" }], next_cursor: null } };
      }
      return { status: "completed", value: { rows: [{ id: "first", name: "First supplier" }], next_cursor: "next" } };
    };
    render(() => (
      <Selector
        disabled={false}
        readOnly={false}
        id="supplier"
        field="value"
        value={id}
        onValueChange={() => undefined}
        transport={{ invoke }}
        reference={PurchaseOrderUpdateForm.presentation.references["change.supplier_id"]}
        label="Supplier"
      />
    ));
    const input = screen.getByRole<HTMLInputElement>("combobox", { name: "Supplier" });
    expect(input.value).not.toContain(id);
    release!();
    await waitFor(() => expect(input.value).toBe("Later supplier"));
  });

  it("uses the generated item label for a narrowed order-line selector", async () => {
    const invoke = vi.fn<Transport["invoke"]>(async () => ({
      status: "completed",
      value: {
        rows: [{ line_id: "line-id", line_number: 42, item_number: "ITEM-HUMAN" }],
      },
    }));
    render(() => (
      <Selector
        disabled={false}
        readOnly={false}
        id="line"
        field="value"
        value="line-id"
        onValueChange={() => undefined}
        transport={{ invoke }}
        reference={ReceivingRecordReceiptForm.presentation.references["value.line[].purchase_order_line_id"]}
        input={{ purchase_order_id: "order-id" }}
        label="Order line"
      />
    ));
    const input = screen.getByRole<HTMLInputElement>("combobox", { name: "Order line" });
    await waitFor(() => expect(input.value).toBe("ITEM-HUMAN"));
    expect(invoke).toHaveBeenCalledWith(expect.objectContaining({ items: [{ purchase_order_id: "order-id" }] }));
  });

  it("does not expose reference keys when lookups fail", async () => {
    const id = "00000000-0000-0000-0000-000000000401";
    const invoke: Transport["invoke"] = async (request): ReturnType<Transport["invoke"]> => {
      if (request.operation === PURCHASE_ORDER_QUERY_ROUTE.operation)
        return {
          status: "completed",
          value: { rows: [{ id: "order", purchase_order_number: "PO-1", supplier_id: id }] },
        };
      throw new Error("Disconnected");
    };
    render(() => (
      <Records screen={props(invoke)} description={PurchaseOrderQueryTable} route={PURCHASE_ORDER_QUERY_ROUTE} />
    ));
    await screen.findByText("Unavailable");
    expect(document.body.textContent).not.toContain(id);
    cleanup();
    render(() => (
      <Selector
        disabled={false}
        readOnly={false}
        id="supplier"
        field="value"
        value={id}
        onValueChange={() => undefined}
        transport={{ invoke }}
        reference={PurchaseOrderUpdateForm.presentation.references["change.supplier_id"]}
        label="Supplier"
      />
    ));
    const input = screen.getByRole<HTMLInputElement>("combobox", { name: "Supplier" });
    await waitFor(() => expect(input.value).toBe("Unavailable"));
    expect(input.value).not.toContain(id);
  });

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
    expectFormActions(PurchaseOrderUpdateForm.submit.label);
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
        recordLink={{ description: { id: "get", title: "Open order" }, path: (row) => `purchase-orders/${row?.id}` }}
      />
    ));
    const table = await screen.findByRole("table", { name: PurchaseOrderQueryTable.title });
    await within(table).findByText("PO-1");
    expect(
      screen.getByRole("button", { name: SupplierCreateForm.title }).closest(".table-workspace-header"),
    ).not.toBeNull();
    const open = within(table).getByRole("link", { name: "PO-1" });
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

  it("sends only the page controls the read declares", async () => {
    // The Receiving screen read has a change feed: its result is a page, but
    // it declares no limit, so the decoder refuses one (wamn-fu8cj).
    const invoke = vi
      .fn<Transport["invoke"]>()
      .mockResolvedValue({ status: "completed", value: { item: [], next_cursor: null, cursor: "snapshot" } });
    render(() => (
      <Records
        screen={props(invoke)}
        description={ReceivingLoadReceiptScreenTable}
        route={RECEIVING_LOAD_RECEIPT_SCREEN_ROUTE}
        input={{ purchase_order_id: "order" }}
      />
    ));
    await waitFor(() => expect(invoke).toHaveBeenCalledTimes(1));
    expect(invoke.mock.calls[0]![0].items).toEqual([{ purchase_order_id: "order" }]);
    cleanup();

    const entries = Array.from({ length: 25 }, (_, index) => ({ id: String(index), cursor: `entry-${index}` }));
    const history = vi
      .fn<Transport["invoke"]>()
      .mockResolvedValueOnce({ status: "completed", value: { rows: entries } })
      .mockResolvedValueOnce({ status: "completed", value: { rows: [] } });
    render(() => (
      <Records
        screen={props(history)}
        description={ReceivingLoadPurchaseOrderHistoryTable}
        route={RECEIVING_LOAD_PURCHASE_ORDER_HISTORY_ROUTE}
        input={{ id: "order" }}
        history
      />
    ));
    await waitFor(() => expect(history).toHaveBeenCalledTimes(1));
    expect(history.mock.calls[0]![0].items).toEqual([{ id: "order", limit: 25 }]);
    await screen.findByText("entry-0");
    fireEvent.click(screen.getByRole("button", { name: "Next page" }));
    await waitFor(() => expect(history).toHaveBeenCalledTimes(2));
    expect(history.mock.calls[1]![0].items).toEqual([{ id: "order", limit: 25, after_cursor: "entry-24" }]);
  });

  it("validates the described supplier form and returns to its table only after the write", async () => {
    const invoke = vi.fn<Transport["invoke"]>(async (request): ReturnType<Transport["invoke"]> => {
      const input = request.items[0] as Record<string, JsonValue>;
      if (!input.idempotency_key)
        return { status: "refused", code: "invalid_input", detail: { field: "idempotency_key" } };
      return { status: "completed", value: { id: "new", name: input.name! } };
    });
    const view = props(invoke);
    render(() => <SupplierForm {...view} />);
    expectFormActions(SupplierCreateForm.title);
    fireEvent.submit(screen.getByRole("button", { name: SupplierCreateForm.title }).closest("form")!);
    await screen.findByText("Enter a value.");
    expect(invoke).not.toHaveBeenCalled();
    fireEvent.input(screen.getByLabelText("Supplier name"), { target: { value: "BROWSER-NEW-SUPPLIER" } });
    flush();
    fireEvent.submit(screen.getByRole("button", { name: SupplierCreateForm.title }).closest("form")!);
    await waitFor(() => expect(view.open).toHaveBeenCalledWith("suppliers"));
    expect(invoke).toHaveBeenCalledWith({
      ...SUPPLIER_CREATE_ROUTE,
      items: [
        {
          id: expect.any(String),
          name: "BROWSER-NEW-SUPPLIER",
          request_id: expect.any(String),
          idempotency_key: expect.any(String),
        },
      ],
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
    expect(writes()[0]!.id).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
    expect(writes()[1]!.id).toBe(writes()[0]!.id);
    expect(writes()[1]!.request_id).not.toBe(writes()[0]!.request_id);
    fireEvent.input(screen.getByLabelText("Supplier name"), { target: { value: "Changed supplier" } });
    flush();
    submit();
    await waitFor(() => expect(view.open).toHaveBeenCalledWith("suppliers"));
    expect(writes()[2]!.idempotency_key).not.toBe(writes()[0]!.idempotency_key);
    expect(writes()[2]!.id).not.toBe(writes()[0]!.id);
    expect(writes()[2]!.name).toBe("Changed supplier");
  });
  it("keeps the receipt intent on retry and replaces it when the operator changes the draft", async () => {
    const writes: JsonValue[] = [];
    const invoke: Transport["invoke"] = async (request): ReturnType<Transport["invoke"]> => {
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
    expectFormActions("Record a receipt");
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
