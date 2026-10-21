/** Receiving's package route table. Descriptions supply every operation label. */
import { createSignal, onCleanup, Show } from "solid-js";
import { DetailView, type DetailDescription } from "@wfe/generator";
import { FieldError } from "@wfe/components";
import { Page, fillPath, type ScreenProps, type ShellSection } from "@wamn/platform-ui";
import type { OperationRoute } from "@wamn/web-runtime";
import * as location from "@wamn/receiving-client/location.js";
import * as purchase from "@wamn/receiving-client/purchase_order.js";
import * as receipt from "@wamn/receiving-client/receipt.js";
import * as receiving from "@wamn/receiving-client/receiving.js";
import * as supplier from "@wamn/receiving-client/supplier.js";
import * as descriptions from "@wamn/receiving-client/components/index.js";
import { Actions, Records, type NavigationAction } from "./table";
import { PurchaseOrderForm, ReceiptForm, SupplierForm } from "./forms";
import { invoke, type RecordValue } from "./data";

const recordPath = (base: string, row?: RecordValue) => `${base}/${encodeURIComponent(String(row?.id ?? ""))}`;
const receiptAction = (fill: (row?: RecordValue) => object): NavigationAction => ({
  description: descriptions.ReceivingRecordReceiptForm,
  path: (row) => fillPath("receipts/new", fill(row)),
});

function Detail(props: {
  screen: ScreenProps;
  description: DetailDescription<RecordValue>;
  route: OperationRoute;
  actions?: readonly NavigationAction[];
}) {
  const [record, setRecord] = createSignal<RecordValue>();
  const [error, setError] = createSignal<string>();
  let active = true;
  onCleanup(() => {
    active = false;
  });
  void invoke<RecordValue>(props.screen.transport, props.route, { id: props.screen.params.id! }).then(
    (record) => {
      if (active) setRecord(record);
    },
    (error) => {
      if (active) setError(String(error));
    },
  );
  return (
    <Page title={props.description.title} actions={<Actions screen={props.screen} actions={props.actions} />}>
      <Show when={error()}>{(text) => <FieldError role="alert">{text()}</FieldError>}</Show>
      <Show when={record()}>
        {(row) => <DetailView description={{ ...props.description, title: undefined }} bindings={{ record: row }} />}
      </Show>
    </Page>
  );
}

const d = descriptions;
export const SECTIONS: readonly ShellSection[] = [
  {
    label: d.PurchaseOrderQueryTable.title,
    screens: [
      {
        scroll: "content",
        path: "purchase-orders",
        operation: d.PurchaseOrderQueryTable.id,
        label: d.PurchaseOrderQueryTable.title,
        component: (props) => (
          <Records
            screen={props}
            description={d.PurchaseOrderQueryTable}
            route={purchase.PURCHASE_ORDER_QUERY_ROUTE}
            filters={["purchase_order_number", "status", "supplier_id"]}
            sortable={["created_at", "purchase_order_number", "status"]}
            rowActions={[
              { description: d.PurchaseOrderGetDetail, path: (row) => recordPath("purchase-orders", row) },
              receiptAction((row) => ({ value: { purchaseOrderId: row?.id } })),
              { description: d.PurchaseOrderUpdateForm, path: (row) => `${recordPath("purchase-orders", row)}/update` },
            ]}
          />
        ),
      },
    ],
    routes: [
      {
        path: "purchase-orders/:id",
        operation: d.PurchaseOrderGetDetail.id,
        label: d.PurchaseOrderGetDetail.title,
        component: (props) => (
          <Detail
            screen={props}
            description={d.PurchaseOrderGetDetail}
            route={purchase.PURCHASE_ORDER_GET_ROUTE}
            actions={[
              receiptAction(() => ({ value: { purchaseOrderId: props.params.id } })),
              {
                description: d.ReceivingLoadReceiptScreenTable,
                path: () => `purchase-orders/${props.params.id}/receiving`,
              },
              {
                description: d.ReceivingLoadPurchaseOrderHistoryTable,
                path: () => `purchase-orders/${props.params.id}/history`,
              },
              { description: d.PurchaseOrderUpdateForm, path: () => `purchase-orders/${props.params.id}/update` },
            ]}
          />
        ),
      },
      {
        path: "purchase-orders/:id/update",
        operation: d.PurchaseOrderUpdateForm.id,
        label: d.PurchaseOrderUpdateForm.title,
        component: PurchaseOrderForm,
      },
      {
        scroll: "content",
        path: "purchase-orders/:id/receiving",
        operation: d.ReceivingLoadReceiptScreenTable.id,
        label: d.ReceivingLoadReceiptScreenTable.title,
        component: (props) => (
          <Records
            screen={props}
            description={d.ReceivingLoadReceiptScreenTable}
            route={receiving.RECEIVING_LOAD_RECEIPT_SCREEN_ROUTE}
            input={{ purchase_order_id: props.params.id! }}
            rowActions={[
              receiptAction((row) => ({
                value: {
                  purchaseOrderId: props.params.id,
                  line: [{ purchaseOrderLineId: row?.line_id }],
                },
              })),
            ]}
          />
        ),
      },
      {
        scroll: "content",
        path: "purchase-orders/:id/history",
        operation: d.ReceivingLoadPurchaseOrderHistoryTable.id,
        label: d.ReceivingLoadPurchaseOrderHistoryTable.title,
        component: (props) => (
          <Records
            screen={props}
            description={d.ReceivingLoadPurchaseOrderHistoryTable}
            route={receiving.RECEIVING_LOAD_PURCHASE_ORDER_HISTORY_ROUTE}
            input={{ id: props.params.id!, limit: 25 }}
            history
          />
        ),
      },
    ],
  },
  {
    label: d.ReceiptQueryTable.title,
    screens: [
      {
        scroll: "content",
        path: "receipts",
        operation: d.ReceiptQueryTable.id,
        label: d.ReceiptQueryTable.title,
        actions: [
          {
            label: d.ReceivingRecordReceiptForm.title,
            path: "receipts/new",
            operation: d.ReceivingRecordReceiptForm.id,
          },
        ],
        component: (props) => (
          <Records
            screen={props}
            description={d.ReceiptQueryTable}
            route={receipt.RECEIPT_QUERY_ROUTE}
            rowActions={[{ description: d.ReceiptGetDetail, path: (row) => recordPath("receipts", row) }]}
          />
        ),
      },
    ],
    routes: [
      {
        path: "receipts/new",
        operation: d.ReceivingRecordReceiptForm.id,
        label: d.ReceivingRecordReceiptForm.title,
        component: ReceiptForm,
      },
      {
        path: "receipts/:id",
        operation: d.ReceiptGetDetail.id,
        label: d.ReceiptGetDetail.title,
        component: (props) => (
          <Detail screen={props} description={d.ReceiptGetDetail} route={receipt.RECEIPT_GET_ROUTE} />
        ),
      },
    ],
  },
  {
    label: d.SupplierQueryTable.title,
    screens: [
      {
        scroll: "content",
        path: "suppliers",
        operation: d.SupplierQueryTable.id,
        label: d.SupplierQueryTable.title,
        actions: [{ label: d.SupplierCreateForm.title, path: "suppliers/new", operation: d.SupplierCreateForm.id }],
        component: (props) => (
          <Records screen={props} description={d.SupplierQueryTable} route={supplier.SUPPLIER_QUERY_ROUTE} />
        ),
      },
    ],
    routes: [
      {
        path: "suppliers/new",
        operation: d.SupplierCreateForm.id,
        label: d.SupplierCreateForm.title,
        component: SupplierForm,
      },
    ],
  },
  {
    label: d.LocationListTable.title,
    screens: [
      {
        scroll: "content",
        path: "locations",
        operation: d.LocationListTable.id,
        label: d.LocationListTable.title,
        component: (props) => (
          <Records
            screen={props}
            description={d.LocationListTable}
            route={location.LOCATION_LIST_ROUTE}
            rowActions={[receiptAction((row) => ({ value: { line: [{ locationId: row?.id }] } }))]}
          />
        ),
      },
    ],
  },
];
