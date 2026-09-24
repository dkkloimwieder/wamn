/**
 * The Receiving page: the generated Receiving components under one transport.
 */

import { Show, createSignal } from "solid-js";

import { Card, CardContent, Field, FieldLabel, Input } from "@wamn/ui";
import type { Outcome, Transport } from "@wamn/web-runtime";
import {
  LocationListTable,
  LocationListTableLabel,
  PurchaseOrderGetDetail,
  PurchaseOrderGetDetailLabel,
  PurchaseOrderQueryTable,
  PurchaseOrderQueryTableLabel,
  PurchaseOrderUpdateForm,
  PurchaseOrderUpdateFormLabel,
  ReceiptGetDetail,
  ReceiptGetDetailLabel,
  ReceiptQueryTable,
  ReceiptQueryTableLabel,
  ReceivingLoadPurchaseOrderHistoryTable,
  ReceivingLoadPurchaseOrderHistoryTableLabel,
  ReceivingLoadReceiptScreenTable,
  ReceivingLoadReceiptScreenTableLabel,
  ReceivingRecordReceiptForm,
  ReceivingRecordReceiptFormLabel,
} from "@wamn/receiving-client/components/index.js";

import { Panel, Waiting } from "./panel.js";

/** The generated screens, in the order an operator meets them. */
export function ReceivingScreens(props: {
  transport: Transport;
  read: (outcome: Outcome<unknown>) => void;
}) {
  const transport = props.transport;
  const read = props.read;
  // One purchase order and one receipt feed every screen that needs a record.
  // A row link writes them, and the operator can also paste one.
  const [order, setOrder] = createSignal("");
  const [receipt, setReceipt] = createSignal("");
  const needsOrder = "Choose a purchase order in the table above.";
  return (
    <>
      <Card size="sm">
        <CardContent class="flex flex-wrap items-end gap-6">
          <Field class="max-w-md">
            <FieldLabel for="demo-order">selected purchase order</FieldLabel>
            <Input
              id="demo-order"
              type="text"
              placeholder="choose a row in Purchase orders"
              value={order()}
              onInput={(event) => setOrder(event.currentTarget.value)}
            />
          </Field>
          <div class="flex flex-col gap-1 pb-2">
            <span class="text-xs uppercase text-muted-foreground">selected receipt</span>
            <span class="font-mono text-sm">{receipt() === "" ? "none" : receipt()}</span>
          </div>
        </CardContent>
      </Card>

      <Panel title={PurchaseOrderQueryTableLabel} operation="purchase_order.query">
        <PurchaseOrderQueryTable
          transport={transport}
          onRowSelect={(row) => setOrder(row.id)}
          onOpenPurchaseOrderGet={(row) => setOrder(row.id)}
          onOutcome={read}
        />
      </Panel>

      <div class="grid gap-6 xl:grid-cols-2">
        <Panel title={PurchaseOrderGetDetailLabel} operation="purchase_order.get">
          <Show when={order() !== ""} fallback={<Waiting>{needsOrder}</Waiting>}>
            <PurchaseOrderGetDetail
              transport={transport}
              input={{ id: order() }}
              onOutcome={read}
            />
          </Show>
        </Panel>

        <Panel title={PurchaseOrderUpdateFormLabel} operation="purchase_order.update">
          <Show when={order() !== ""} fallback={<Waiting>{needsOrder}</Waiting>}>
            <PurchaseOrderUpdateForm
              transport={transport}
              key={{ id: order() }}
              onSubmitted={read}
            />
          </Show>
        </Panel>
      </div>

      <Panel title={ReceivingRecordReceiptFormLabel} operation="receiving.record_receipt">
        <Show when={order() !== ""} fallback={<Waiting>{needsOrder}</Waiting>}>
          <ReceivingRecordReceiptForm
            transport={transport}
            initial={{ value: { purchaseOrderId: order() } }}
            onSubmitted={read}
          />
        </Show>
      </Panel>

      <Panel title={ReceivingLoadReceiptScreenTableLabel} operation="receiving.load_receipt_screen">
        <Show when={order() !== ""} fallback={<Waiting>{needsOrder}</Waiting>}>
          <ReceivingLoadReceiptScreenTable
            transport={transport}
            fixed={{ purchaseOrderId: order() }}
            onOutcome={read}
          />
        </Show>
      </Panel>

      <Panel
        title={ReceivingLoadPurchaseOrderHistoryTableLabel}
        operation="receiving.load_purchase_order_history"
      >
        <Show when={order() !== ""} fallback={<Waiting>{needsOrder}</Waiting>}>
          <ReceivingLoadPurchaseOrderHistoryTable
            transport={transport}
            fixed={{ id: order(), limit: 20 }}
            onOutcome={read}
          />
        </Show>
      </Panel>

      <div class="grid gap-6 xl:grid-cols-2">
        <Panel title={ReceiptQueryTableLabel} operation="receipt.query">
          <ReceiptQueryTable
            transport={transport}
            onRowSelect={(row) => setReceipt(row.id)}
            onOpenReceiptGet={(row) => setReceipt(row.id)}
            onOutcome={read}
          />
        </Panel>

        <Panel title={ReceiptGetDetailLabel} operation="receipt.get">
          <Show
            when={receipt() !== ""}
            fallback={<Waiting>Choose a receipt in the table beside this one.</Waiting>}
          >
            <ReceiptGetDetail transport={transport} input={{ id: receipt() }} onOutcome={read} />
          </Show>
        </Panel>
      </div>

      <Panel title={LocationListTableLabel} operation="location.list">
        <LocationListTable transport={transport} onOutcome={read} />
      </Panel>
    </>
  );
}
