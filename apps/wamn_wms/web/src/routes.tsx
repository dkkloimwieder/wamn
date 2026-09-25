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
 */

import type { ScreenProps, ShellSection } from "@wamn/shell";
import {
  InventoryAggregateTable,
  InventoryAggregateTableLabel,
  InventoryMovementGetDetail,
  InventoryMovementQueryTable,
  InventoryMovementQueryTableLabel,
  LocationGetDetail,
  LocationQueryTable,
  LocationQueryTableLabel,
  PalletGetDetail,
  PalletQuantityGetDetail,
  PalletQuantityQueryTable,
  PalletQuantityQueryTableLabel,
  PalletQueryTable,
  PalletQueryTableLabel,
  ProductGetDetail,
  ProductQueryTable,
  ProductQueryTableLabel,
} from "@wamn/wms-client/components/index.js";

/** The record page path of one row. */
const record = (path: string, row: { readonly id: string }) => `${path}/${encodeURIComponent(row.id)}`;

/** The key of the record page, from the address. The route path always holds it. */
const key = (props: ScreenProps) => ({ id: props.params.id ?? "" });

export const SECTIONS: readonly ShellSection[] = [
  {
    label: "Pallets",
    screens: [
      {
        path: "pallets",
        label: PalletQueryTableLabel,
        component: (props) => (
          <PalletQueryTable
            transport={props.transport}
            onOpenPalletGet={(row) => props.open(record("pallets", row))}
          />
        ),
      },
    ],
    records: [
      {
        path: "pallets/:id",
        component: (props) => <PalletGetDetail transport={props.transport} input={key(props)} />,
      },
    ],
  },
  {
    label: "Pallet quantities",
    screens: [
      {
        path: "pallet-quantities",
        label: PalletQuantityQueryTableLabel,
        component: (props) => (
          <PalletQuantityQueryTable
            transport={props.transport}
            onOpenPalletQuantityGet={(row) => props.open(record("pallet-quantities", row))}
          />
        ),
      },
    ],
    records: [
      {
        path: "pallet-quantities/:id",
        component: (props) => <PalletQuantityGetDetail transport={props.transport} input={key(props)} />,
      },
    ],
  },
  {
    label: "Inventory",
    screens: [
      {
        path: "inventory",
        label: InventoryAggregateTableLabel,
        component: (props) => <InventoryAggregateTable transport={props.transport} />,
      },
    ],
  },
  {
    label: "Inventory movements",
    screens: [
      {
        path: "inventory-movements",
        label: InventoryMovementQueryTableLabel,
        component: (props) => (
          <InventoryMovementQueryTable
            transport={props.transport}
            onOpenInventoryMovementGet={(row) => props.open(record("inventory-movements", row))}
          />
        ),
      },
    ],
    records: [
      {
        path: "inventory-movements/:id",
        component: (props) => <InventoryMovementGetDetail transport={props.transport} input={key(props)} />,
      },
    ],
  },
  {
    label: "Locations",
    screens: [
      {
        path: "locations",
        label: LocationQueryTableLabel,
        component: (props) => (
          <LocationQueryTable
            transport={props.transport}
            onOpenLocationGet={(row) => props.open(record("locations", row))}
          />
        ),
      },
    ],
    records: [
      {
        path: "locations/:id",
        component: (props) => <LocationGetDetail transport={props.transport} input={key(props)} />,
      },
    ],
  },
  {
    label: "Products",
    screens: [
      {
        path: "products",
        label: ProductQueryTableLabel,
        component: (props) => (
          <ProductQueryTable
            transport={props.transport}
            onOpenProductGet={(row) => props.open(record("products", row))}
          />
        ),
      },
    ],
    records: [
      {
        path: "products/:id",
        component: (props) => <ProductGetDetail transport={props.transport} input={key(props)} />,
      },
    ],
  },
];
