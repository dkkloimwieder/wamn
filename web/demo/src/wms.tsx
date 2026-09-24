/**
 * The WMS page: every generated WMS component under one transport.
 *
 * The page holds one selected record of each model. A row selection writes it,
 * and the detail and update screens read it. A row action that opens a command
 * form writes that form's starting values, and the form mounts again with them.
 */

import { Show, createSignal } from "solid-js";

import { Card, CardContent } from "@wamn/ui";
import type { Outcome, Transport } from "@wamn/web-runtime";
import {
  InventoryAdjustForm,
  InventoryAdjustFormLabel,
  InventoryAggregateTable,
  InventoryAggregateTableLabel,
  InventoryMergeForm,
  InventoryMergeFormLabel,
  InventoryMoveForm,
  InventoryMoveFormLabel,
  InventoryMovementGetDetail,
  InventoryMovementGetDetailLabel,
  InventoryMovementQueryTable,
  InventoryMovementQueryTableLabel,
  InventorySplitForm,
  InventorySplitFormLabel,
  LocationCreateForm,
  LocationCreateFormLabel,
  LocationGetDetail,
  LocationGetDetailLabel,
  LocationQueryTable,
  LocationQueryTableLabel,
  LocationUpdateForm,
  LocationUpdateFormLabel,
  PalletCreateForm,
  PalletCreateFormLabel,
  PalletGetDetail,
  PalletGetDetailLabel,
  PalletQuantityGetDetail,
  PalletQuantityGetDetailLabel,
  PalletQuantityQueryTable,
  PalletQuantityQueryTableLabel,
  PalletQueryTable,
  PalletQueryTableLabel,
  ProductCreateForm,
  ProductCreateFormLabel,
  ProductGetDetail,
  ProductGetDetailLabel,
  ProductQueryTable,
  ProductQueryTableLabel,
  ProductUpdateForm,
  ProductUpdateFormLabel,
  type InventoryAdjustFormInitial,
  type InventoryMoveFormInitial,
  type InventorySplitFormInitial,
  type PalletCreateFormInitial,
} from "@wamn/wms-client/components/index.js";

import { Panel, Waiting } from "./panel.js";

/** The pallet a command acts on, with the revision the operator read. */
interface PickedPallet {
  readonly id: string;
  readonly code: string;
  readonly rowVersion: number;
}

/**
 * Two sets of starting values, merged one level down.
 *
 * Two row actions can fill one form, for example a pallet and then a
 * location for a move, so a later fill keeps what an earlier one wrote.
 */
function merge<T extends object>(earlier: T, later: T): T {
  const merged: Record<string, unknown> = { ...(earlier as Record<string, unknown>) };
  for (const [key, value] of Object.entries(later)) {
    const before = merged[key];
    merged[key] =
      typeof before === "object" && before !== null && typeof value === "object" && value !== null
        ? { ...before, ...value }
        : value;
  }
  return merged as T;
}

/** The generated screens, grouped by model. */
export function WmsScreens(props: {
  transport: Transport;
  read: (outcome: Outcome<unknown>) => void;
}) {
  const transport = props.transport;
  const read = props.read;
  const [pallet, setPallet] = createSignal<PickedPallet | null>(null);
  const [location, setLocation] = createSignal("");
  const [product, setProduct] = createSignal("");
  const [quantity, setQuantity] = createSignal("");
  const [movement, setMovement] = createSignal("");
  // Each fill is a new object, so a keyed Show mounts the form again with it.
  const [moveFill, setMoveFill] = createSignal<InventoryMoveFormInitial>({});
  const [adjustFill, setAdjustFill] = createSignal<InventoryAdjustFormInitial>({});
  const [splitFill, setSplitFill] = createSignal<InventorySplitFormInitial>({});
  const [palletFill, setPalletFill] = createSignal<PalletCreateFormInitial>({});

  const pickPallet = (row: { id: string; palletCode: string; rowVersion: number }) =>
    setPallet({ id: row.id, code: row.palletCode, rowVersion: row.rowVersion });
  // The release binds no read that supplies a command's revision, so the
  // page hands each command the revision of the pallet row the operator
  // selected.
  const needsPallet = "Select a pallet row. A command sends the revision of that row.";
  const needsTarget = "Select the target pallet row. Merge sends the target's revision.";

  return (
    <>
      <Card size="sm">
        <CardContent class="flex flex-wrap gap-6 font-mono text-sm">
          <span>
            pallet: {pallet() === null ? "none" : `${pallet()?.code} at revision ${pallet()?.rowVersion}`}
          </span>
          <span>location: {location() === "" ? "none" : location()}</span>
          <span>product: {product() === "" ? "none" : product()}</span>
        </CardContent>
      </Card>

      <Panel title={PalletQueryTableLabel} operation="pallet.query">
        <PalletQueryTable
          transport={transport}
          onRowSelect={pickPallet}
          onOpenPalletGet={pickPallet}
          onFillInventoryMove={(initial) => setMoveFill(merge(moveFill(), initial))}
          onFillInventoryAdjust={(initial) => setAdjustFill(merge(adjustFill(), initial))}
          onFillInventorySplit={(initial) => setSplitFill(merge(splitFill(), initial))}
          onOutcome={read}
        />
      </Panel>

      <div class="grid gap-6 xl:grid-cols-2">
        <Panel title={PalletGetDetailLabel} operation="pallet.get">
          <Show when={pallet()} keyed fallback={<Waiting>Select a pallet row.</Waiting>}>
            {(picked) => (
              <PalletGetDetail transport={transport} input={{ id: picked.id }} onOutcome={read} />
            )}
          </Show>
        </Panel>

        <Panel title={PalletCreateFormLabel} operation="pallet.create">
          <Show when={palletFill()} keyed>
            {(initial) => (
              <PalletCreateForm transport={transport} initial={initial} onSubmitted={read} />
            )}
          </Show>
        </Panel>
      </div>

      <div class="grid gap-6 xl:grid-cols-2">
        <Panel title={InventoryMoveFormLabel} operation="inventory.move">
          <Show when={pallet()} fallback={<Waiting>{needsPallet}</Waiting>}>
            <Show when={moveFill()} keyed>
              {(initial) => (
                <InventoryMoveForm
                  transport={transport}
                  initial={initial}
                  valueExpectedRowVersion={pallet()?.rowVersion ?? 0}
                  onSubmitted={read}
                />
              )}
            </Show>
          </Show>
        </Panel>

        <Panel title={InventoryAdjustFormLabel} operation="inventory.adjust">
          <Show when={pallet()} fallback={<Waiting>{needsPallet}</Waiting>}>
            <Show when={adjustFill()} keyed>
              {(initial) => (
                <InventoryAdjustForm
                  transport={transport}
                  initial={initial}
                  valueExpectedRowVersion={pallet()?.rowVersion ?? 0}
                  onSubmitted={read}
                />
              )}
            </Show>
          </Show>
        </Panel>

        <Panel title={InventoryMergeFormLabel} operation="inventory.merge">
          {/* Merge names a pallet twice, so no row fills it, and it guards the
              target's revision: select the target row, choose both pallets. */}
          <Show when={pallet()} fallback={<Waiting>{needsTarget}</Waiting>}>
            <InventoryMergeForm
              transport={transport}
              valueExpectedRowVersion={pallet()?.rowVersion ?? 0}
              onSubmitted={read}
            />
          </Show>
        </Panel>

        <Panel title={InventorySplitFormLabel} operation="inventory.split">
          <Show when={pallet()} fallback={<Waiting>{needsPallet}</Waiting>}>
            <Show when={splitFill()} keyed>
              {(initial) => (
                <InventorySplitForm
                  transport={transport}
                  initial={initial}
                  valueExpectedRowVersion={pallet()?.rowVersion ?? 0}
                  onSubmitted={read}
                />
              )}
            </Show>
          </Show>
        </Panel>
      </div>

      <Panel title={InventoryAggregateTableLabel} operation="inventory.aggregate">
        <InventoryAggregateTable transport={transport} onOutcome={read} />
      </Panel>

      <Panel title={LocationQueryTableLabel} operation="location.query">
        <LocationQueryTable
          transport={transport}
          onRowSelect={(row) => setLocation(row.id)}
          onOpenLocationGet={(row) => setLocation(row.id)}
          onFillInventoryMove={(initial) => setMoveFill(merge(moveFill(), initial))}
          onFillInventorySplit={(initial) => setSplitFill(merge(splitFill(), initial))}
          onFillPalletCreate={(initial) => setPalletFill(merge(palletFill(), initial))}
          onOutcome={read}
        />
      </Panel>

      <div class="grid gap-6 xl:grid-cols-3">
        <Panel title={LocationGetDetailLabel} operation="location.get">
          <Show when={location() !== ""} fallback={<Waiting>Select a location row.</Waiting>}>
            <LocationGetDetail transport={transport} input={{ id: location() }} onOutcome={read} />
          </Show>
        </Panel>

        <Panel title={LocationUpdateFormLabel} operation="location.update">
          <Show when={location() !== ""} fallback={<Waiting>Select a location row.</Waiting>}>
            <LocationUpdateForm transport={transport} key={{ id: location() }} onSubmitted={read} />
          </Show>
        </Panel>

        <Panel title={LocationCreateFormLabel} operation="location.create">
          <LocationCreateForm transport={transport} onSubmitted={read} />
        </Panel>
      </div>

      <Panel title={ProductQueryTableLabel} operation="product.query">
        <ProductQueryTable
          transport={transport}
          onRowSelect={(row) => setProduct(row.id)}
          onOpenProductGet={(row) => setProduct(row.id)}
          onFillInventoryAdjust={(initial) => setAdjustFill(merge(adjustFill(), initial))}
          onFillInventorySplit={(initial) => setSplitFill(merge(splitFill(), initial))}
          onOutcome={read}
        />
      </Panel>

      <div class="grid gap-6 xl:grid-cols-3">
        <Panel title={ProductGetDetailLabel} operation="product.get">
          <Show when={product() !== ""} fallback={<Waiting>Select a product row.</Waiting>}>
            <ProductGetDetail transport={transport} input={{ id: product() }} onOutcome={read} />
          </Show>
        </Panel>

        <Panel title={ProductUpdateFormLabel} operation="product.update">
          <Show when={product() !== ""} fallback={<Waiting>Select a product row.</Waiting>}>
            <ProductUpdateForm transport={transport} key={{ id: product() }} onSubmitted={read} />
          </Show>
        </Panel>

        <Panel title={ProductCreateFormLabel} operation="product.create">
          <ProductCreateForm transport={transport} onSubmitted={read} />
        </Panel>
      </div>

      <div class="grid gap-6 xl:grid-cols-2">
        <Panel title={PalletQuantityQueryTableLabel} operation="pallet_quantity.query">
          <PalletQuantityQueryTable
            transport={transport}
            onRowSelect={(row) => setQuantity(row.id)}
            onOpenPalletQuantityGet={(row) => setQuantity(row.id)}
            onOutcome={read}
          />
        </Panel>

        <Panel title={PalletQuantityGetDetailLabel} operation="pallet_quantity.get">
          <Show when={quantity() !== ""} fallback={<Waiting>Select a quantity row.</Waiting>}>
            <PalletQuantityGetDetail transport={transport} input={{ id: quantity() }} onOutcome={read} />
          </Show>
        </Panel>
      </div>

      <div class="grid gap-6 xl:grid-cols-2">
        <Panel title={InventoryMovementQueryTableLabel} operation="inventory_movement.query">
          <InventoryMovementQueryTable
            transport={transport}
            onRowSelect={(row) => setMovement(row.id)}
            onOpenInventoryMovementGet={(row) => setMovement(row.id)}
            onOutcome={read}
          />
        </Panel>

        <Panel title={InventoryMovementGetDetailLabel} operation="inventory_movement.get">
          <Show when={movement() !== ""} fallback={<Waiting>Select a movement row.</Waiting>}>
            <InventoryMovementGetDetail transport={transport} input={{ id: movement() }} onOutcome={read} />
          </Show>
        </Panel>
      </div>
    </>
  );
}
