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

import { createResource, Show, type JSX } from "solid-js";

import { fillPath, filledValues, type ScreenProps, type ShellSection } from "@wamn/shell";
import type { Transport } from "@wamn/web-runtime";
import {
  InventoryAdjustForm,
  InventoryAggregateTable,
  InventoryAggregateTableLabel,
  InventoryMergeForm,
  InventoryMergeFormLabel,
  InventoryMoveForm,
  InventoryMovementGetDetail,
  InventoryMovementQueryTable,
  InventoryMovementQueryTableLabel,
  InventorySplitForm,
  LocationCreateForm,
  LocationCreateFormLabel,
  LocationGetDetail,
  LocationQueryTable,
  LocationQueryTableLabel,
  LocationUpdateForm,
  LocationUpdateFormLabel,
  PalletCreateForm,
  PalletCreateFormLabel,
  PalletGetDetail,
  PalletQuantityGetDetail,
  PalletQuantityQueryTable,
  PalletQuantityQueryTableLabel,
  PalletQueryTable,
  PalletQueryTableLabel,
  ProductCreateForm,
  ProductCreateFormLabel,
  ProductGetDetail,
  ProductQueryTable,
  ProductQueryTableLabel,
  ProductUpdateForm,
  ProductUpdateFormLabel,
} from "@wamn/wms-client/components/index.js";
import { get as palletGet } from "@wamn/wms-client/pallet.js";

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

/**
 * Reads the pallet a command names, and renders the form with the revision it
 * read. The release binds no read that supplies the revision of these
 * commands, so the form sends the revision of the pallet in the address.
 */
function PalletRevision(props: {
  readonly transport: Transport;
  readonly id: string | undefined;
  readonly children: (rowVersion: number) => JSX.Element;
}): JSX.Element {
  const [read] = createResource(
    () => props.id,
    (id) => palletGet(props.transport, [{ id }]),
  );
  return (
    <Show when={props.id} fallback={<p>Open this form from a pallet row. It sends the revision of that pallet.</p>}>
      <Show when={read()} keyed>
        {(outcome) =>
          outcome.status === "completed" ? (
            props.children(outcome.value.rowVersion)
          ) : (
            <p>The pallet could not be read: {outcome.status}.</p>
          )
        }
      </Show>
    </Show>
  );
}

export const SECTIONS: readonly ShellSection[] = [
  {
    label: "Pallets",
    screens: [
      {
        path: "pallets",
        label: PalletQueryTableLabel,
        actions: [{ label: PalletCreateFormLabel, path: "pallets/new" }],
        component: (props) => (
          <PalletQueryTable
            transport={props.transport}
            onOpenPalletGet={(row) => props.open(record("pallets", row))}
            onFillInventoryMove={(fill) => props.open(fillPath("inventory/move", fill))}
            onFillInventoryAdjust={(fill) => props.open(fillPath("inventory/adjust", fill))}
            onFillInventorySplit={(fill) => props.open(fillPath("inventory/split", fill))}
          />
        ),
      },
    ],
    routes: [
      {
        path: "pallets/new",
        component: (props) => (
          <PalletCreateForm transport={props.transport} initial={filledValues(props.search)} onSubmitted={done(props)} />
        ),
      },
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
    routes: [
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
        actions: [{ label: InventoryMergeFormLabel, path: "inventory/merge" }],
        component: (props) => <InventoryAggregateTable transport={props.transport} />,
      },
    ],
    routes: [
      {
        path: "inventory/move",
        component: (props) => (
          <PalletRevision transport={props.transport} id={props.search["value.palletId"]}>
            {(rowVersion) => (
              <InventoryMoveForm
                transport={props.transport}
                initial={filledValues(props.search)}
                valueExpectedRowVersion={rowVersion}
                onSubmitted={done(props)}
              />
            )}
          </PalletRevision>
        ),
      },
      {
        path: "inventory/adjust",
        component: (props) => (
          <PalletRevision transport={props.transport} id={props.search["value.palletId"]}>
            {(rowVersion) => (
              <InventoryAdjustForm
                transport={props.transport}
                initial={filledValues(props.search)}
                valueExpectedRowVersion={rowVersion}
                onSubmitted={done(props)}
              />
            )}
          </PalletRevision>
        ),
      },
      {
        path: "inventory/split",
        component: (props) => (
          <PalletRevision transport={props.transport} id={props.search["value.sourcePalletId"]}>
            {(rowVersion) => (
              <InventorySplitForm
                transport={props.transport}
                initial={filledValues(props.search)}
                valueExpectedRowVersion={rowVersion}
                onSubmitted={done(props)}
              />
            )}
          </PalletRevision>
        ),
      },
      {
        path: "inventory/merge",
        component: (props) => <InventoryMergeForm transport={props.transport} onSubmitted={done(props)} />,
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
    routes: [
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
        actions: [{ label: LocationCreateFormLabel, path: "locations/new" }],
        component: (props) => (
          <LocationQueryTable
            transport={props.transport}
            onOpenLocationGet={(row) => props.open(record("locations", row))}
            onFillPalletCreate={(fill) => props.open(fillPath("pallets/new", fill))}
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
            onOpenProductGet={(row) => props.open(record("products", row))}
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
