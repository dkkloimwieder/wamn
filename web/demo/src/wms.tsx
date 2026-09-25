/** WMS inventory and packaging operations through the generated contracts. */
import { Show, createSignal } from "solid-js";
import type { Outcome, Transport } from "@wamn/web-runtime";
import { Panel, Waiting } from "./panel.js";
import {
  InventoryQueryTable,
  InventoryQueryTableLabel,
  InventoryGetDetail,
  InventoryGetDetailLabel,
  PackagingQueryTable,
  PackagingQueryTableLabel,
  PackagingGetDetail,
  PackagingGetDetailLabel,
  InventoryTransactionQueryTable,
  InventoryTransactionQueryTableLabel,
  InventoryTransactionGetDetail,
  InventoryTransactionGetDetailLabel,
  ProductQueryTable,
  ProductQueryTableLabel,
  ProductGetDetail,
  ProductGetDetailLabel,
  LocationQueryTable,
  LocationQueryTableLabel,
  LocationGetDetail,
  LocationGetDetailLabel,
  InventoryMoveForm,
  InventoryAdjustForm,
  InventorySplitForm,
  InventoryMergeForm,
  PackagingCreateForm,
  PackagingCloseForm,
  ProductCreateForm,
  ProductUpdateForm,
  LocationCreateForm,
  LocationUpdateForm,
  InventoryMoveFormLabel,
  InventoryAdjustFormLabel,
  InventorySplitFormLabel,
  InventoryMergeFormLabel,
  PackagingCreateFormLabel,
  PackagingCloseFormLabel,
  ProductCreateFormLabel,
  ProductUpdateFormLabel,
  LocationCreateFormLabel,
  LocationUpdateFormLabel,
  InventoryAggregateTable,
  InventoryAggregateTableLabel,
} from "@wamn/wms-client/components/index.js";

export function WmsScreens(props: { transport: Transport; read: (outcome: Outcome<unknown>) => void }) {
  const [inventory, setInventory] = createSignal<{ id: string; rowVersion?: number } | null>(null);
  const [packaging, setPackaging] = createSignal<{ id: string; rowVersion?: number } | null>(null);
  const [inventoryTransaction, setInventoryTransaction] = createSignal<{ id: string; rowVersion?: number } | null>(null);
  const [product, setProduct] = createSignal<{ id: string; rowVersion?: number } | null>(null);
  const [location, setLocation] = createSignal<{ id: string; rowVersion?: number } | null>(null);
  const [mergeTarget, setMergeTarget] = createSignal<{ id: string; rowVersion?: number } | null>(null);
  const [destination, setDestination] = createSignal<{ id: string; locationId: string } | null>(null);
  return <>
    <Panel title={InventoryQueryTableLabel} operation="inventory.query">
      <InventoryQueryTable transport={props.transport} onRowSelect={setInventory} onOutcome={props.read} />
    </Panel>
    <Panel title={InventoryGetDetailLabel} operation="inventory.get">
      <Show when={inventory()} keyed fallback={<Waiting>Select a inventory row.</Waiting>}>
        {row => <InventoryGetDetail transport={props.transport} input={{ id: row.id }} onOutcome={props.read} />}
      </Show>
    </Panel>
    <Panel title={PackagingQueryTableLabel} operation="packaging.query">
      <PackagingQueryTable transport={props.transport} onRowSelect={row => { setPackaging(row); setDestination(row); }} onOutcome={props.read} />
    </Panel>
    <Panel title={PackagingGetDetailLabel} operation="packaging.get">
      <Show when={packaging()} keyed fallback={<Waiting>Select a packaging row.</Waiting>}>
        {row => <PackagingGetDetail transport={props.transport} input={{ id: row.id }} onOutcome={props.read} />}
      </Show>
    </Panel>
    <Panel title={InventoryTransactionQueryTableLabel} operation="inventory_transaction.query">
      <InventoryTransactionQueryTable transport={props.transport} onRowSelect={setInventoryTransaction} onOutcome={props.read} />
    </Panel>
    <Panel title={InventoryTransactionGetDetailLabel} operation="inventory_transaction.get">
      <Show when={inventoryTransaction()} keyed fallback={<Waiting>Select a inventory transaction row.</Waiting>}>
        {row => <InventoryTransactionGetDetail transport={props.transport} input={{ id: row.id }} onOutcome={props.read} />}
      </Show>
    </Panel>
    <Panel title={ProductQueryTableLabel} operation="product.query">
      <ProductQueryTable transport={props.transport} onRowSelect={setProduct} onOutcome={props.read} />
    </Panel>
    <Panel title={ProductGetDetailLabel} operation="product.get">
      <Show when={product()} keyed fallback={<Waiting>Select a product row.</Waiting>}>
        {row => <ProductGetDetail transport={props.transport} input={{ id: row.id }} onOutcome={props.read} />}
      </Show>
    </Panel>
    <Panel title={LocationQueryTableLabel} operation="location.query">
      <LocationQueryTable transport={props.transport} onRowSelect={setLocation} onOutcome={props.read} />
    </Panel>
    <Panel title={LocationGetDetailLabel} operation="location.get">
      <Show when={location()} keyed fallback={<Waiting>Select a location row.</Waiting>}>
        {row => <LocationGetDetail transport={props.transport} input={{ id: row.id }} onOutcome={props.read} />}
      </Show>
    </Panel>
    <Panel title={InventoryMoveFormLabel} operation="inventory.move">
      <Show when={inventory() && { value: { inventoryId: inventory()!.id, ...(destination() ? { toPackagingId: destination()!.id, toLocationId: destination()!.locationId } : {}) } }} keyed fallback={<Waiting>Select a inventory row.</Waiting>}>
        {initial => <InventoryMoveForm transport={props.transport} valueExpectedRowVersion={inventory()!.rowVersion!} initial={initial} onSubmitted={props.read} />}
      </Show>
    </Panel>
    <Panel title={InventoryAdjustFormLabel} operation="inventory.adjust">
      <Show when={inventory() && { value: { inventoryId: inventory()!.id } }} keyed fallback={<Waiting>Select a inventory row.</Waiting>}>
        {initial => <InventoryAdjustForm transport={props.transport} valueExpectedRowVersion={inventory()!.rowVersion!} initial={initial} onSubmitted={props.read} />}
      </Show>
    </Panel>
    <Panel title={InventorySplitFormLabel} operation="inventory.split">
      <Show when={inventory() && { value: { fromInventoryId: inventory()!.id, ...(destination() ? { toPackagingId: destination()!.id, toLocationId: destination()!.locationId } : {}) } }} keyed fallback={<Waiting>Select a inventory row.</Waiting>}>
        {initial => <InventorySplitForm transport={props.transport} valueExpectedRowVersion={inventory()!.rowVersion!} initial={initial} onSubmitted={props.read} />}
      </Show>
    </Panel>
    <Panel title={InventoryMergeFormLabel} operation="inventory.merge">
      <button type="button" disabled={!inventory()} onClick={() => setMergeTarget(inventory())}>Use selected inventory as merge target</button>
      <p>Select another inventory row as the source. Target: {mergeTarget()?.id ?? "none"}</p>
      <Show when={inventory() && mergeTarget() && { value: { fromInventoryId: inventory()!.id, toInventoryId: mergeTarget()!.id } }} keyed>
        {initial => <InventoryMergeForm transport={props.transport} initial={initial}
          valueExpectedFromRowVersion={inventory()!.rowVersion!} valueExpectedToRowVersion={mergeTarget()!.rowVersion!} onSubmitted={props.read} />}
      </Show>
    </Panel>
    <Panel title={PackagingCreateFormLabel} operation="packaging.create"><PackagingCreateForm transport={props.transport} onSubmitted={props.read} /></Panel>
    <Panel title={PackagingCloseFormLabel} operation="packaging.close">
      <Show when={packaging() && { value: { packagingId: packaging()!.id } }} keyed fallback={<Waiting>Select a packaging row.</Waiting>}>
        {initial => <PackagingCloseForm transport={props.transport} valueExpectedRowVersion={packaging()!.rowVersion!} initial={initial} onSubmitted={props.read} />}
      </Show>
    </Panel>
    <Panel title={ProductCreateFormLabel} operation="product.create"><ProductCreateForm transport={props.transport} onSubmitted={props.read} /></Panel>
    <Panel title={ProductUpdateFormLabel} operation="product.update">
      <Show when={product()} keyed fallback={<Waiting>Select a product row.</Waiting>}>
        {row => <ProductUpdateForm transport={props.transport} key={{ id: row.id }} onSubmitted={props.read} />}
      </Show>
    </Panel>
    <Panel title={LocationCreateFormLabel} operation="location.create"><LocationCreateForm transport={props.transport} onSubmitted={props.read} /></Panel>
    <Panel title={LocationUpdateFormLabel} operation="location.update">
      <Show when={location()} keyed fallback={<Waiting>Select a location row.</Waiting>}>
        {row => <LocationUpdateForm transport={props.transport} key={{ id: row.id }} onSubmitted={props.read} />}
      </Show>
    </Panel>
    <Panel title={InventoryAggregateTableLabel} operation="inventory.aggregate"><InventoryAggregateTable transport={props.transport} onOutcome={props.read} /></Panel>
  </>;
}
