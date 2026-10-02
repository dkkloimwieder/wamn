/**
 * The route table of the Receiving web application.
 *
 * A section label names the model, and each label below it is the label the
 * generated module exports. Each screen hands its component the transport of
 * the signed-in session, which the shell hands it as a prop.
 *
 * A purchase order and a receipt have a record page at `<path>/<id>`, which a
 * table row opens. The purchase order page opens its update form, the receipt
 * form, its receiving screen and its history, each on a route below it. The
 * receipt form also opens from the Receipts table and from a purchase order
 * row, which fills the order in the query. A row of the receiving screen
 * fills the order and one receipt line, and a location row fills the location
 * of one receipt line. A completed submission returns to the page that opened
 * the form.
 */

import { Show } from "solid-js";

import { fillPath, filledValues, screen, type ScreenProps, type ShellSection } from "@wamn/shell";
import {
  LocationListTableLabel,
  PurchaseOrderQueryTableLabel,
  PurchaseOrderUpdateFormLabel,
  ReceiptQueryTableLabel,
  ReceivingLoadPurchaseOrderHistoryTableLabel,
  ReceivingLoadReceiptScreenTableLabel,
  ReceivingRecordReceiptFormLabel,
  SupplierCreateFormLabel,
  SupplierQueryTableLabel,
} from "@wamn/receiving-client/components/labels.js";
import { LOCATION_LIST_ROUTE } from "@wamn/receiving-client/location.js";
import {
  PURCHASE_ORDER_GET_ROUTE,
  PURCHASE_ORDER_QUERY_ROUTE,
  PURCHASE_ORDER_UPDATE_ROUTE,
} from "@wamn/receiving-client/purchase_order.js";
import { RECEIPT_GET_ROUTE, RECEIPT_QUERY_ROUTE } from "@wamn/receiving-client/receipt.js";
import {
  RECEIVING_LOAD_PURCHASE_ORDER_HISTORY_ROUTE,
  RECEIVING_LOAD_RECEIPT_SCREEN_ROUTE,
  RECEIVING_RECORD_RECEIPT_ROUTE,
} from "@wamn/receiving-client/receiving.js";
import { SUPPLIER_CREATE_ROUTE, SUPPLIER_QUERY_ROUTE } from "@wamn/receiving-client/supplier.js";

/** The generated module of each model. A route loads its module when it opens. */
const location = () => import("@wamn/receiving-client/components/location.js");
const purchaseOrder = () => import("@wamn/receiving-client/components/purchase_order.js");
const receipt = () => import("@wamn/receiving-client/components/receipt.js");
const receiving = () => import("@wamn/receiving-client/components/receiving.js");
const supplier = () => import("@wamn/receiving-client/components/supplier.js");

/** The record page path of one row. */
const record = (path: string, row: { readonly id: string }) => `${path}/${encodeURIComponent(row.id)}`;

/** The key of the record page, from the address. The route path always holds it. */
// eslint-disable-next-line solid/reactivity -- every caller reads it inside a JSX prop, whose getter its component tracks.
const key = (props: ScreenProps) => ({ id: props.params.id ?? "" });

/** Returns to the page that opened the form once a submission completes. A refusal stays on the form. */
const done = (props: ScreenProps) => (outcome: { readonly status: string }) => {
  if (outcome.status === "completed") {
    props.close();
  }
};

/** The number of history entries on one page. */
const HISTORY_PAGE = 20;

export const SECTIONS: readonly ShellSection[] = [
  {
    label: "Purchase orders",
    screens: [
      {
        path: "purchase-orders",
        operation: PURCHASE_ORDER_QUERY_ROUTE.operation,
        label: PurchaseOrderQueryTableLabel,
        component: screen(purchaseOrder, (m) => (props) => (
          <m.PurchaseOrderQueryTable
            transport={props.transport}
            onOpen={{ "wamn-receiving:purchase-order/get": (row) => props.open(record("purchase-orders", row)) }}
            onFill={{
              "wamn-receiving:receiving/record-receipt": (fill) => props.open(fillPath("receipts/new", fill)),
            }}
          />
        )),
      },
    ],
    routes: [
      {
        path: "purchase-orders/:id",
        operation: PURCHASE_ORDER_GET_ROUTE.operation,
        actions: [
          {
            label: ReceivingRecordReceiptFormLabel,
            path: "receipts/new?value.purchaseOrderId=:id",
            operation: RECEIVING_RECORD_RECEIPT_ROUTE.operation,
          },
          {
            label: ReceivingLoadReceiptScreenTableLabel,
            path: "purchase-orders/:id/receiving",
            operation: RECEIVING_LOAD_RECEIPT_SCREEN_ROUTE.operation,
          },
          {
            label: ReceivingLoadPurchaseOrderHistoryTableLabel,
            path: "purchase-orders/:id/history",
            operation: RECEIVING_LOAD_PURCHASE_ORDER_HISTORY_ROUTE.operation,
          },
          {
            label: PurchaseOrderUpdateFormLabel,
            path: "purchase-orders/:id/update",
            operation: PURCHASE_ORDER_UPDATE_ROUTE.operation,
          },
        ],
        component: screen(purchaseOrder, (m) => (props) => (
          <m.PurchaseOrderGetDetail transport={props.transport} input={key(props)} />
        )),
      },
      {
        path: "purchase-orders/:id/update",
        operation: PURCHASE_ORDER_UPDATE_ROUTE.operation,
        component: screen(purchaseOrder, (m) => (props) => (
          <m.PurchaseOrderUpdateForm transport={props.transport} key={key(props)} onSubmitted={done(props)} />
        )),
      },
      {
        path: "purchase-orders/:id/receiving",
        operation: RECEIVING_LOAD_RECEIPT_SCREEN_ROUTE.operation,
        // A table reads its fixed input once, so a new order in the address
        // mounts a new table (wamn-erwv.6).
        component: screen(receiving, (m) => (props) => (
          <Show when={key(props).id} keyed>
            {(id) => (
              <m.ReceivingLoadReceiptScreenTable
                transport={props.transport}
                fixed={{ purchaseOrderId: id }}
                onFill={{
                  "wamn-receiving:receiving/record-receipt": (fill: { readonly value?: object }) =>
                    props.open(fillPath("receipts/new", { ...fill, value: { ...fill.value, purchaseOrderId: id } })),
                }}
              />
            )}
          </Show>
        )),
      },
      {
        path: "purchase-orders/:id/history",
        operation: RECEIVING_LOAD_PURCHASE_ORDER_HISTORY_ROUTE.operation,
        // A new order in the address mounts a new table, as above (wamn-erwv.6).
        component: screen(receiving, (m) => (props) => (
          <Show when={key(props).id} keyed>
            {(id) => (
              <m.ReceivingLoadPurchaseOrderHistoryTable
                transport={props.transport}
                fixed={{ id, limit: HISTORY_PAGE }}
              />
            )}
          </Show>
        )),
      },
    ],
  },
  {
    label: "Receipts",
    screens: [
      {
        path: "receipts",
        operation: RECEIPT_QUERY_ROUTE.operation,
        label: ReceiptQueryTableLabel,
        actions: [
          {
            label: ReceivingRecordReceiptFormLabel,
            path: "receipts/new",
            operation: RECEIVING_RECORD_RECEIPT_ROUTE.operation,
          },
        ],
        component: screen(receipt, (m) => (props) => (
          <m.ReceiptQueryTable
            transport={props.transport}
            onOpen={{ "wamn-receiving:receipt/get": (row) => props.open(record("receipts", row)) }}
          />
        )),
      },
    ],
    routes: [
      {
        path: "receipts/new",
        operation: RECEIVING_RECORD_RECEIPT_ROUTE.operation,
        component: screen(receiving, (m) => (props) => (
          <m.ReceivingRecordReceiptForm
            transport={props.transport}
            initial={filledValues(props.search)}
            onSubmitted={done(props)}
          />
        )),
      },
      {
        path: "receipts/:id",
        operation: RECEIPT_GET_ROUTE.operation,
        component: screen(receipt, (m) => (props) => (
          <m.ReceiptGetDetail transport={props.transport} input={key(props)} />
        )),
      },
    ],
  },
  {
    label: "Suppliers",
    screens: [
      {
        path: "suppliers",
        operation: SUPPLIER_QUERY_ROUTE.operation,
        label: SupplierQueryTableLabel,
        actions: [
          { label: SupplierCreateFormLabel, path: "suppliers/new", operation: SUPPLIER_CREATE_ROUTE.operation },
        ],
        component: screen(supplier, (m) => (props) => <m.SupplierQueryTable transport={props.transport} />),
      },
    ],
    routes: [
      {
        path: "suppliers/new",
        operation: SUPPLIER_CREATE_ROUTE.operation,
        component: screen(supplier, (m) => (props) => (
          <m.SupplierCreateForm
            transport={props.transport}
            initial={filledValues(props.search)}
            onSubmitted={done(props)}
          />
        )),
      },
    ],
  },
  {
    label: "Locations",
    screens: [
      {
        path: "locations",
        operation: LOCATION_LIST_ROUTE.operation,
        label: LocationListTableLabel,
        component: screen(location, (m) => (props) => (
          <m.LocationListTable
            transport={props.transport}
            onFill={{
              "wamn-receiving:receiving/record-receipt": (fill) => props.open(fillPath("receipts/new", fill)),
            }}
          />
        )),
      },
    ],
  },
];
