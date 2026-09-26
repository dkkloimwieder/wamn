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

import { fillPath, filledValues, type ScreenProps, type ShellSection } from "@wamn/shell";
import {
  LocationListTable,
  LocationListTableLabel,
  PurchaseOrderGetDetail,
  PurchaseOrderQueryTable,
  PurchaseOrderQueryTableLabel,
  PurchaseOrderUpdateForm,
  PurchaseOrderUpdateFormLabel,
  ReceiptGetDetail,
  ReceiptQueryTable,
  ReceiptQueryTableLabel,
  ReceivingLoadPurchaseOrderHistoryTable,
  ReceivingLoadPurchaseOrderHistoryTableLabel,
  ReceivingLoadReceiptScreenTable,
  ReceivingLoadReceiptScreenTableLabel,
  ReceivingRecordReceiptForm,
  ReceivingRecordReceiptFormLabel,
  SupplierCreateForm,
  SupplierCreateFormLabel,
  SupplierQueryTable,
  SupplierQueryTableLabel,
} from "@wamn/receiving-client/components/index.js";

/** The record page path of one row. */
const record = (path: string, row: { readonly id: string }) => `${path}/${encodeURIComponent(row.id)}`;

/** The key of the record page, from the address. The route path always holds it. */
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
        label: PurchaseOrderQueryTableLabel,
        component: (props) => (
          <PurchaseOrderQueryTable
            transport={props.transport}
            onOpen={{ "wamn-receiving:purchase-order/get@1.0.0": (row) => props.open(record("purchase-orders", row)) }}
            onFill={{ "wamn-receiving:receiving/record-receipt@1.0.0": (fill) => props.open(fillPath("receipts/new", fill)) }}
          />
        ),
      },
    ],
    routes: [
      {
        path: "purchase-orders/:id",
        actions: [
          { label: ReceivingRecordReceiptFormLabel, path: "receipts/new?value.purchaseOrderId=:id" },
          { label: ReceivingLoadReceiptScreenTableLabel, path: "purchase-orders/:id/receiving" },
          { label: ReceivingLoadPurchaseOrderHistoryTableLabel, path: "purchase-orders/:id/history" },
          { label: PurchaseOrderUpdateFormLabel, path: "purchase-orders/:id/update" },
        ],
        component: (props) => <PurchaseOrderGetDetail transport={props.transport} input={key(props)} />,
      },
      {
        path: "purchase-orders/:id/update",
        component: (props) => (
          <PurchaseOrderUpdateForm transport={props.transport} key={key(props)} onSubmitted={done(props)} />
        ),
      },
      {
        path: "purchase-orders/:id/receiving",
        component: (props) => (
          <ReceivingLoadReceiptScreenTable
            transport={props.transport}
            fixed={{ purchaseOrderId: key(props).id }}
            onFill={{
              "wamn-receiving:receiving/record-receipt@1.0.0": (fill: { readonly value?: object }) =>
                props.open(
                  fillPath("receipts/new", { ...fill, value: { ...fill.value, purchaseOrderId: key(props).id } }),
                ),
            }}
          />
        ),
      },
      {
        path: "purchase-orders/:id/history",
        component: (props) => (
          <ReceivingLoadPurchaseOrderHistoryTable
            transport={props.transport}
            fixed={{ id: key(props).id, limit: HISTORY_PAGE }}
          />
        ),
      },
    ],
  },
  {
    label: "Receipts",
    screens: [
      {
        path: "receipts",
        label: ReceiptQueryTableLabel,
        actions: [{ label: ReceivingRecordReceiptFormLabel, path: "receipts/new" }],
        component: (props) => (
          <ReceiptQueryTable
            transport={props.transport}
            onOpen={{ "wamn-receiving:receipt/get@1.0.0": (row) => props.open(record("receipts", row)) }}
          />
        ),
      },
    ],
    routes: [
      {
        path: "receipts/new",
        component: (props) => (
          <ReceivingRecordReceiptForm
            transport={props.transport}
            initial={filledValues(props.search)}
            onSubmitted={done(props)}
          />
        ),
      },
      {
        path: "receipts/:id",
        component: (props) => <ReceiptGetDetail transport={props.transport} input={key(props)} />,
      },
    ],
  },
  {
    label: "Suppliers",
    screens: [
      {
        path: "suppliers",
        label: SupplierQueryTableLabel,
        actions: [{ label: SupplierCreateFormLabel, path: "suppliers/new" }],
        component: (props) => <SupplierQueryTable transport={props.transport} />,
      },
    ],
    routes: [
      {
        path: "suppliers/new",
        component: (props) => (
          <SupplierCreateForm
            transport={props.transport}
            initial={filledValues(props.search)}
            onSubmitted={done(props)}
          />
        ),
      },
    ],
  },
  {
    label: "Locations",
    screens: [
      {
        path: "locations",
        label: LocationListTableLabel,
        component: (props) => (
          <LocationListTable
            transport={props.transport}
            onFill={{ "wamn-receiving:receiving/record-receipt@1.0.0": (fill) => props.open(fillPath("receipts/new", fill)) }}
          />
        ),
      },
    ],
  },
];
