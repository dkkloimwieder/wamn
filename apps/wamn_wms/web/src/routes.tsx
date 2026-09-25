/**
 * The route table of the WMS web application: one screen for each table.
 *
 * A group label names the model, and an entry label is the screen label the
 * generated module exports. Each screen hands its component the transport of
 * the signed-in session, which the shell hands it as a prop.
 */

import type { ShellSection } from "@wamn/shell";
import {
  InventoryAggregateTable,
  InventoryAggregateTableLabel,
  InventoryMovementQueryTable,
  InventoryMovementQueryTableLabel,
  LocationQueryTable,
  LocationQueryTableLabel,
  PalletQuantityQueryTable,
  PalletQuantityQueryTableLabel,
  PalletQueryTable,
  PalletQueryTableLabel,
  ProductQueryTable,
  ProductQueryTableLabel,
} from "@wamn/wms-client/components/index.js";

export const SECTIONS: readonly ShellSection[] = [
  {
    label: "Pallets",
    screens: [
      {
        path: "pallets",
        label: PalletQueryTableLabel,
        component: (props) => <PalletQueryTable transport={props.transport} />,
      },
    ],
  },
  {
    label: "Pallet quantities",
    screens: [
      {
        path: "pallet-quantities",
        label: PalletQuantityQueryTableLabel,
        component: (props) => <PalletQuantityQueryTable transport={props.transport} />,
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
        component: (props) => <InventoryMovementQueryTable transport={props.transport} />,
      },
    ],
  },
  {
    label: "Locations",
    screens: [
      {
        path: "locations",
        label: LocationQueryTableLabel,
        component: (props) => <LocationQueryTable transport={props.transport} />,
      },
    ],
  },
  {
    label: "Products",
    screens: [
      {
        path: "products",
        label: ProductQueryTableLabel,
        component: (props) => <ProductQueryTable transport={props.transport} />,
      },
    ],
  },
];
