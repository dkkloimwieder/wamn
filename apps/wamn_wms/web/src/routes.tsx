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

import { fillPath, filledValues, screen, type ScreenProps, type ShellSection } from "@wamn/shell";
import {
  InventoryAggregateTableLabel,
  InventoryMergeFormLabel,
  InventoryTransactionQueryTableLabel,
  LocationCreateFormLabel,
  LocationQueryTableLabel,
  LocationUpdateFormLabel,
  PackagingCreateFormLabel,
  PackagingQuantityQueryTableLabel,
  PackagingQueryTableLabel,
  ProductCreateFormLabel,
  ProductQueryTableLabel,
  ProductUpdateFormLabel,
} from "@wamn/wms-client/components/labels.js";
import {
  INVENTORY_ADJUST_ROUTE,
  INVENTORY_AGGREGATE_ROUTE,
  INVENTORY_MERGE_ROUTE,
  INVENTORY_MOVE_ROUTE,
  INVENTORY_SPLIT_ROUTE,
} from "@wamn/wms-client/inventory.js";
import {
  INVENTORY_TRANSACTION_GET_ROUTE,
  INVENTORY_TRANSACTION_QUERY_ROUTE,
} from "@wamn/wms-client/inventory_transaction.js";
import {
  LOCATION_CREATE_ROUTE,
  LOCATION_GET_ROUTE,
  LOCATION_QUERY_ROUTE,
  LOCATION_UPDATE_ROUTE,
} from "@wamn/wms-client/location.js";
import { PACKAGING_CREATE_ROUTE, PACKAGING_GET_ROUTE, PACKAGING_QUERY_ROUTE } from "@wamn/wms-client/packaging.js";
import { PACKAGING_QUANTITY_GET_ROUTE, PACKAGING_QUANTITY_QUERY_ROUTE } from "@wamn/wms-client/packaging_quantity.js";
import {
  PRODUCT_CREATE_ROUTE,
  PRODUCT_GET_ROUTE,
  PRODUCT_QUERY_ROUTE,
  PRODUCT_UPDATE_ROUTE,
} from "@wamn/wms-client/product.js";

/** The generated module of each model. A route loads its module when it opens. */
const inventory = () => import("@wamn/wms-client/components/inventory.js");
const inventoryTransaction = () => import("@wamn/wms-client/components/inventory_transaction.js");
const location = () => import("@wamn/wms-client/components/location.js");
const packaging = () => import("@wamn/wms-client/components/packaging.js");
const packagingQuantity = () => import("@wamn/wms-client/components/packaging_quantity.js");
const product = () => import("@wamn/wms-client/components/product.js");

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

export const SECTIONS: readonly ShellSection[] = [
  {
    label: "Packagings",
    screens: [
      {
        path: "packagings",
        operation: PACKAGING_QUERY_ROUTE.operation,
        label: PackagingQueryTableLabel,
        actions: [
          { label: PackagingCreateFormLabel, path: "packagings/new", operation: PACKAGING_CREATE_ROUTE.operation },
        ],
        component: screen(packaging, (m) => (props) => (
          <m.PackagingQueryTable
            transport={props.transport}
            onOpen={{ "wamn-wms:packaging/get": (row) => props.open(record("packagings", row)) }}
            onFill={{
              "wamn-wms:inventory/move": (fill) => props.open(fillPath("inventory/move", fill)),
              "wamn-wms:inventory/adjust": (fill) => props.open(fillPath("inventory/adjust", fill)),
              "wamn-wms:inventory/split": (fill) => props.open(fillPath("inventory/split", fill)),
            }}
          />
        )),
      },
    ],
    routes: [
      {
        path: "packagings/new",
        operation: PACKAGING_CREATE_ROUTE.operation,
        component: screen(packaging, (m) => (props) => (
          <m.PackagingCreateForm
            transport={props.transport}
            initial={filledValues(props.search)}
            onSubmitted={done(props)}
          />
        )),
      },
      {
        path: "packagings/:id",
        operation: PACKAGING_GET_ROUTE.operation,
        component: screen(packaging, (m) => (props) => (
          <m.PackagingGetDetail transport={props.transport} input={key(props)} />
        )),
      },
    ],
  },
  {
    label: "Packaging quantities",
    screens: [
      {
        path: "packaging-quantities",
        operation: PACKAGING_QUANTITY_QUERY_ROUTE.operation,
        label: PackagingQuantityQueryTableLabel,
        component: screen(packagingQuantity, (m) => (props) => (
          <m.PackagingQuantityQueryTable
            transport={props.transport}
            onOpen={{
              "wamn-wms:packaging-quantity/get": (row) => props.open(record("packaging-quantities", row)),
            }}
          />
        )),
      },
    ],
    routes: [
      {
        path: "packaging-quantities/:id",
        operation: PACKAGING_QUANTITY_GET_ROUTE.operation,
        component: screen(packagingQuantity, (m) => (props) => (
          <m.PackagingQuantityGetDetail transport={props.transport} input={key(props)} />
        )),
      },
    ],
  },
  {
    label: "Inventory",
    screens: [
      {
        path: "inventory",
        operation: INVENTORY_AGGREGATE_ROUTE.operation,
        label: InventoryAggregateTableLabel,
        actions: [
          { label: InventoryMergeFormLabel, path: "inventory/merge", operation: INVENTORY_MERGE_ROUTE.operation },
        ],
        component: screen(inventory, (m) => (props) => <m.InventoryAggregateTable transport={props.transport} />),
      },
    ],
    routes: [
      {
        path: "inventory/move",
        operation: INVENTORY_MOVE_ROUTE.operation,
        component: screen(inventory, (m) => (props) => (
          <m.InventoryMoveForm
            transport={props.transport}
            initial={filledValues(props.search)}
            onSubmitted={done(props)}
          />
        )),
      },
      {
        path: "inventory/adjust",
        operation: INVENTORY_ADJUST_ROUTE.operation,
        component: screen(inventory, (m) => (props) => (
          <m.InventoryAdjustForm
            transport={props.transport}
            initial={filledValues(props.search)}
            onSubmitted={done(props)}
          />
        )),
      },
      {
        path: "inventory/split",
        operation: INVENTORY_SPLIT_ROUTE.operation,
        component: screen(inventory, (m) => (props) => (
          <m.InventorySplitForm
            transport={props.transport}
            initial={filledValues(props.search)}
            onSubmitted={done(props)}
          />
        )),
      },
      {
        path: "inventory/merge",
        operation: INVENTORY_MERGE_ROUTE.operation,
        component: screen(inventory, (m) => (props) => (
          <m.InventoryMergeForm transport={props.transport} onSubmitted={done(props)} />
        )),
      },
    ],
  },
  {
    label: "Inventory transactions",
    screens: [
      {
        path: "inventory-transactions",
        operation: INVENTORY_TRANSACTION_QUERY_ROUTE.operation,
        label: InventoryTransactionQueryTableLabel,
        component: screen(inventoryTransaction, (m) => (props) => (
          <m.InventoryTransactionQueryTable
            transport={props.transport}
            onOpen={{
              "wamn-wms:inventory-transaction/get": (row) => props.open(record("inventory-transactions", row)),
            }}
          />
        )),
      },
    ],
    routes: [
      {
        path: "inventory-transactions/:id",
        operation: INVENTORY_TRANSACTION_GET_ROUTE.operation,
        component: screen(inventoryTransaction, (m) => (props) => (
          <m.InventoryTransactionGetDetail transport={props.transport} input={key(props)} />
        )),
      },
    ],
  },
  {
    label: "Locations",
    screens: [
      {
        path: "locations",
        operation: LOCATION_QUERY_ROUTE.operation,
        label: LocationQueryTableLabel,
        actions: [
          { label: LocationCreateFormLabel, path: "locations/new", operation: LOCATION_CREATE_ROUTE.operation },
        ],
        component: screen(location, (m) => (props) => (
          <m.LocationQueryTable
            transport={props.transport}
            onOpen={{ "wamn-wms:location/get": (row) => props.open(record("locations", row)) }}
            onFill={{ "wamn-wms:packaging/create": (fill) => props.open(fillPath("packagings/new", fill)) }}
          />
        )),
      },
    ],
    routes: [
      {
        path: "locations/new",
        operation: LOCATION_CREATE_ROUTE.operation,
        component: screen(location, (m) => (props) => (
          <m.LocationCreateForm
            transport={props.transport}
            initial={filledValues(props.search)}
            onSubmitted={done(props)}
          />
        )),
      },
      {
        path: "locations/:id",
        operation: LOCATION_GET_ROUTE.operation,
        actions: [
          { label: LocationUpdateFormLabel, path: "locations/:id/update", operation: LOCATION_UPDATE_ROUTE.operation },
        ],
        component: screen(location, (m) => (props) => (
          <m.LocationGetDetail transport={props.transport} input={key(props)} />
        )),
      },
      {
        path: "locations/:id/update",
        operation: LOCATION_UPDATE_ROUTE.operation,
        component: screen(location, (m) => (props) => (
          <m.LocationUpdateForm transport={props.transport} key={key(props)} onSubmitted={done(props)} />
        )),
      },
    ],
  },
  {
    label: "Products",
    screens: [
      {
        path: "products",
        operation: PRODUCT_QUERY_ROUTE.operation,
        label: ProductQueryTableLabel,
        actions: [{ label: ProductCreateFormLabel, path: "products/new", operation: PRODUCT_CREATE_ROUTE.operation }],
        component: screen(product, (m) => (props) => (
          <m.ProductQueryTable
            transport={props.transport}
            onOpen={{ "wamn-wms:product/get": (row) => props.open(record("products", row)) }}
          />
        )),
      },
    ],
    routes: [
      {
        path: "products/new",
        operation: PRODUCT_CREATE_ROUTE.operation,
        component: screen(product, (m) => (props) => (
          <m.ProductCreateForm
            transport={props.transport}
            initial={filledValues(props.search)}
            onSubmitted={done(props)}
          />
        )),
      },
      {
        path: "products/:id",
        operation: PRODUCT_GET_ROUTE.operation,
        actions: [
          { label: ProductUpdateFormLabel, path: "products/:id/update", operation: PRODUCT_UPDATE_ROUTE.operation },
        ],
        component: screen(product, (m) => (props) => (
          <m.ProductGetDetail transport={props.transport} input={key(props)} />
        )),
      },
      {
        path: "products/:id/update",
        operation: PRODUCT_UPDATE_ROUTE.operation,
        component: screen(product, (m) => (props) => (
          <m.ProductUpdateForm transport={props.transport} key={key(props)} onSubmitted={done(props)} />
        )),
      },
    ],
  },
];
