/**
 * The route table of the WMS web application: one screen for each table.
 *
 * A section label names the model, and a screen label is the label the
 * generated module exports. The navigation shows the screen label only for a
 * model with more than one screen. Each screen hands its component the
 * transport of the signed-in session, which the shell hands it as a prop.
 *
 * A model with a get detail has a record page at `<path>/<id>`. The get button
 * of a table row opens it, and the page reads the record by the id in the
 * address, so a reload shows the same record.
 *
 * Each form has its own route. A create or merge form opens from a button
 * above its table, and an update form from a button above its record page. A
 * row button that fills a form opens it with the row values in the query, so a
 * reload keeps them. A completed submission returns to the page that opened
 * the form.
 */

import { fillPath, filledValues, type ScreenProps, type ShellSection } from "@wamn/shell";
import {
  InventoryAdjustForm,
  InventoryAggregateTable,
  InventoryAggregateTableLabel,
  InventoryMergeForm,
  InventoryMergeFormLabel,
  InventoryMoveForm,
  InventoryTransactionGetDetail,
  InventoryTransactionQueryTable,
  InventoryTransactionQueryTableLabel,
  InventorySplitForm,
  LocationCreateForm,
  LocationCreateFormLabel,
  LocationGetDetail,
  LocationQueryTable,
  LocationQueryTableLabel,
  LocationUpdateForm,
  LocationUpdateFormLabel,
  PackagingCreateForm,
  PackagingCreateFormLabel,
  PackagingGetDetail,
  PackagingQuantityGetDetail,
  PackagingQuantityQueryTable,
  PackagingQuantityQueryTableLabel,
  PackagingQueryTable,
  PackagingQueryTableLabel,
  ProductCreateForm,
  ProductCreateFormLabel,
  ProductGetDetail,
  ProductQueryTable,
  ProductQueryTableLabel,
  ProductUpdateForm,
  ProductUpdateFormLabel,
} from "@wamn/wms-client/components/index.js";

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

export const SECTIONS: readonly ShellSection[] = [
  {
    label: "Packagings",
    screens: [
      {
        path: "packagings",
        label: PackagingQueryTableLabel,
        actions: [{ label: PackagingCreateFormLabel, path: "packagings/new" }],
        component: (props) => (
          <PackagingQueryTable
            transport={props.transport}
            onOpen={{ "wamn-wms:packaging/get@1.0.0": (row) => props.open(record("packagings", row)) }}
            onFill={{
              "wamn-wms:inventory/move@1.0.0": (fill) => props.open(fillPath("inventory/move", fill)),
              "wamn-wms:inventory/adjust@1.0.0": (fill) => props.open(fillPath("inventory/adjust", fill)),
              "wamn-wms:inventory/split@1.0.0": (fill) => props.open(fillPath("inventory/split", fill)),
            }}
          />
        ),
      },
    ],
    routes: [
      {
        path: "packagings/new",
        component: (props) => (
          <PackagingCreateForm transport={props.transport} initial={filledValues(props.search)} onSubmitted={done(props)} />
        ),
      },
      {
        path: "packagings/:id",
        component: (props) => <PackagingGetDetail transport={props.transport} input={key(props)} />,
      },
    ],
  },
  {
    label: "Packaging quantities",
    screens: [
      {
        path: "packaging-quantities",
        label: PackagingQuantityQueryTableLabel,
        component: (props) => (
          <PackagingQuantityQueryTable
            transport={props.transport}
            onOpen={{ "wamn-wms:packaging-quantity/get@1.0.0": (row) => props.open(record("packaging-quantities", row)) }}
          />
        ),
      },
    ],
    routes: [
      {
        path: "packaging-quantities/:id",
        component: (props) => <PackagingQuantityGetDetail transport={props.transport} input={key(props)} />,
      },
    ],
  },
  {
    label: "Inventory",
    screens: [
      {
        path: "inventory",
        label: InventoryAggregateTableLabel,
        actions: [{ label: InventoryMergeFormLabel, path: "inventory/merge" }],
        component: (props) => <InventoryAggregateTable transport={props.transport} />,
      },
    ],
    routes: [
      {
        path: "inventory/move",
        component: (props) => (
          <InventoryMoveForm transport={props.transport} initial={filledValues(props.search)} onSubmitted={done(props)} />
        ),
      },
      {
        path: "inventory/adjust",
        component: (props) => (
          <InventoryAdjustForm transport={props.transport} initial={filledValues(props.search)} onSubmitted={done(props)} />
        ),
      },
      {
        path: "inventory/split",
        component: (props) => (
          <InventorySplitForm transport={props.transport} initial={filledValues(props.search)} onSubmitted={done(props)} />
        ),
      },
      {
        path: "inventory/merge",
        component: (props) => <InventoryMergeForm transport={props.transport} onSubmitted={done(props)} />,
      },
    ],
  },
  {
    label: "Inventory transactions",
    screens: [
      {
        path: "inventory-transactions",
        label: InventoryTransactionQueryTableLabel,
        component: (props) => (
          <InventoryTransactionQueryTable
            transport={props.transport}
            onOpen={{
              "wamn-wms:inventory-transaction/get@1.0.0": (row) => props.open(record("inventory-transactions", row)),
            }}
          />
        ),
      },
    ],
    routes: [
      {
        path: "inventory-transactions/:id",
        component: (props) => <InventoryTransactionGetDetail transport={props.transport} input={key(props)} />,
      },
    ],
  },
  {
    label: "Locations",
    screens: [
      {
        path: "locations",
        label: LocationQueryTableLabel,
        actions: [{ label: LocationCreateFormLabel, path: "locations/new" }],
        component: (props) => (
          <LocationQueryTable
            transport={props.transport}
            onOpen={{ "wamn-wms:location/get@1.0.0": (row) => props.open(record("locations", row)) }}
            onFill={{ "wamn-wms:packaging/create@1.0.0": (fill) => props.open(fillPath("packagings/new", fill)) }}
          />
        ),
      },
    ],
    routes: [
      {
        path: "locations/new",
        component: (props) => (
          <LocationCreateForm transport={props.transport} initial={filledValues(props.search)} onSubmitted={done(props)} />
        ),
      },
      {
        path: "locations/:id",
        actions: [{ label: LocationUpdateFormLabel, path: "locations/:id/update" }],
        component: (props) => <LocationGetDetail transport={props.transport} input={key(props)} />,
      },
      {
        path: "locations/:id/update",
        component: (props) => (
          <LocationUpdateForm transport={props.transport} key={key(props)} onSubmitted={done(props)} />
        ),
      },
    ],
  },
  {
    label: "Products",
    screens: [
      {
        path: "products",
        label: ProductQueryTableLabel,
        actions: [{ label: ProductCreateFormLabel, path: "products/new" }],
        component: (props) => (
          <ProductQueryTable
            transport={props.transport}
            onOpen={{ "wamn-wms:product/get@1.0.0": (row) => props.open(record("products", row)) }}
          />
        ),
      },
    ],
    routes: [
      {
        path: "products/new",
        component: (props) => (
          <ProductCreateForm transport={props.transport} initial={filledValues(props.search)} onSubmitted={done(props)} />
        ),
      },
      {
        path: "products/:id",
        actions: [{ label: ProductUpdateFormLabel, path: "products/:id/update" }],
        component: (props) => <ProductGetDetail transport={props.transport} input={key(props)} />,
      },
      {
        path: "products/:id/update",
        component: (props) => (
          <ProductUpdateForm transport={props.transport} key={key(props)} onSubmitted={done(props)} />
        ),
      },
    ],
  },
];
