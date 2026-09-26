// @generated from the client-contract IR; do not edit.
//
// `inventory_movement` components. Each one calls the bindings and the runtime, and
// nothing else.

import { Show, createResource, onCleanup } from "solid-js";
import {
  afterWrites,
  cellText,
  readMember,
  type Outcome,
  type Transport,
  type Uuid,
  writeMember,
} from "@wamn/web-runtime";
import {
  Button,
  DataTable,
  DetailItem,
  DetailList,
  FieldError,
  TableScreen,
  announceOutcome,
  createRecordLabels,
  createTableLoad,
} from "@wamn/ui";
import {
  get,
  query,
  type InventoryMovementGetRequest,
  type InventoryMovementGetResult,
  type InventoryMovementQueryRequest,
  type InventoryMovementQueryResult,
  type InventoryMovementQueryRow,
} from "../inventory_movement.js";
import {
  get as locationGet,
  type LocationGetRequest,
} from "../location.js";
import {
  get as palletGet,
  type PalletGetRequest,
} from "../pallet.js";
import {
  get as productGet,
  type ProductGetRequest,
} from "../product.js";

/** The record that the detail for `wamn-wms:inventory-movement/get@1.0.0` reads. */
export interface InventoryMovementGetDetailInput {
  readonly id: Uuid;
}

/** What the detail screen for `wamn-wms:inventory-movement/get@1.0.0` takes. */
export interface InventoryMovementGetDetailProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** The input that names the record. */
  readonly input: InventoryMovementGetDetailInput;
  /** Called with every outcome this screen reads. */
  readonly onOutcome?: (outcome: Outcome<InventoryMovementGetResult>) => void;
}

/** What an operator calls this screen. The page decides where it goes. */
export const InventoryMovementGetDetailLabel = "get";

/**
 * The detail screen for `wamn-wms:inventory-movement/get@1.0.0`.
 *
 * It reads when it mounts and again whenever its input changes, because the
 * input names the record it shows.
 */
export function InventoryMovementGetDetail(props: InventoryMovementGetDetailProps) {
  const [outcome, { refetch: readAgain }] = createResource(
    () => props.input,
    async (input: InventoryMovementGetDetailInput) => {
      const read = await get(props.transport, [
        input as InventoryMovementGetRequest,
      ]);
      props.onOutcome?.(read);
      if (read.status !== "completed") {
        announceOutcome(read, InventoryMovementGetDetailLabel);
      }
      return read;
    },
  );
  onCleanup(afterWrites(props.transport, () => void readAgain()));
  const record = (): InventoryMovementGetResult | undefined => {
    const read = outcome();
    return read?.status === "completed" ? read.value : undefined;
  };
  const state = () => outcome()?.status;

  return (
    <section>
      <Show when={state() !== undefined && state() !== "completed"}>
        <FieldError>{state()}</FieldError>
      </Show>
      <DetailList loading={outcome.loading}>
        <DetailItem term="created at">{cellText(readMember(record(), ["createdAt"]), "timestamptz")}</DetailItem>
        <DetailItem term="created by">{cellText(readMember(record(), ["createdBy"]), "uuid")}</DetailItem>
        <DetailItem term="from location id">{cellText(readMember(record(), ["fromLocationId"]), "uuid")}</DetailItem>
        <DetailItem term="id">{cellText(readMember(record(), ["id"]), "uuid")}</DetailItem>
        <DetailItem term="idempotency key">{cellText(readMember(record(), ["idempotencyKey"]), "text")}</DetailItem>
        <DetailItem term="kind">{cellText(readMember(record(), ["kind"]), "text")}</DetailItem>
        <DetailItem term="occurred at">{cellText(readMember(record(), ["occurredAt"]), "timestamptz")}</DetailItem>
        <DetailItem term="pallet id">{cellText(readMember(record(), ["palletId"]), "uuid")}</DetailItem>
        <DetailItem term="product id">{cellText(readMember(record(), ["productId"]), "uuid")}</DetailItem>
        <DetailItem term="quantity">{cellText(readMember(record(), ["quantity"]), "numeric")}</DetailItem>
        <DetailItem term="reason code">{cellText(readMember(record(), ["reasonCode"]), "text")}</DetailItem>
        <DetailItem term="to location id">{cellText(readMember(record(), ["toLocationId"]), "uuid")}</DetailItem>
      </DetailList>
    </section>
  );
}

/** What the table for `wamn-wms:inventory-movement/query@1.0.0` takes. */
export interface InventoryMovementQueryTableProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** Input the parent fixes, which the operator does not edit. */
  readonly fixed?: Partial<InventoryMovementQueryRequest>;
  /** Called when the operator opens `wamn-wms:inventory-movement/get@1.0.0` from one row. */
  readonly onOpenInventoryMovementGet?: (row: InventoryMovementQueryRow) => void;
  /** Called with every outcome this screen reads. */
  readonly onOutcome?: (outcome: Outcome<InventoryMovementQueryResult>) => void;
}

/** What an operator calls this screen. The page decides where it goes. */
export const InventoryMovementQueryTableLabel = "query";

/**
 * The table for `wamn-wms:inventory-movement/query@1.0.0`: the DataTable over `INVENTORY_MOVEMENT_QUERY_TABLE`.
 *
 * It loads when it mounts. A change to a filter, a sort of rows the load did
 * not read in full, a cap change and a refresh each start a new load.
 */
export function InventoryMovementQueryTable(props: InventoryMovementQueryTableProps) {
  const load = createTableLoad<InventoryMovementQueryRow>(INVENTORY_MOVEMENT_QUERY_TABLE, async (limit) => {
    let request = { ...props.fixed } as InventoryMovementQueryRequest;
    request = writeMember(request, ["limit"], limit) as InventoryMovementQueryRequest;
    const outcome = await query(props.transport, [request]);
    props.onOutcome?.(outcome);
    if (outcome.status !== "completed") {
      announceOutcome(outcome, InventoryMovementQueryTableLabel);
    }
    return outcome;
  });
  void load.load();
  onCleanup(afterWrites(props.transport, () => void load.load()));

  const locationGetLabels = createRecordLabels(async (key) => {
    const request = writeMember({}, ["id"], key) as LocationGetRequest;
    const outcome = await locationGet(props.transport, [request]);
    if (outcome.status !== "completed") {
      return null;
    }
    const text = outcome.value.locationCode;
    return text == null ? null : String(text);
  });
  const palletGetLabels = createRecordLabels(async (key) => {
    const request = writeMember({}, ["id"], key) as PalletGetRequest;
    const outcome = await palletGet(props.transport, [request]);
    if (outcome.status !== "completed") {
      return null;
    }
    const text = outcome.value.palletCode;
    return text == null ? null : String(text);
  });
  const productGetLabels = createRecordLabels(async (key) => {
    const request = writeMember({}, ["id"], key) as ProductGetRequest;
    const outcome = await productGet(props.transport, [request]);
    if (outcome.status !== "completed") {
      return null;
    }
    const text = outcome.value.productCode;
    return text == null ? null : String(text);
  });

  const columns = INVENTORY_MOVEMENT_QUERY_TABLE.columns.map((column) => {
    switch (column.field) {
      case "fromLocationId":
        return { ...column, cell: (value: unknown) => <>{locationGetLabels(value as string | null)}</> };
      case "palletId":
        return { ...column, cell: (value: unknown) => <>{palletGetLabels(value as string | null)}</> };
      case "productId":
        return { ...column, cell: (value: unknown) => <>{productGetLabels(value as string | null)}</> };
      case "toLocationId":
        return { ...column, cell: (value: unknown) => <>{locationGetLabels(value as string | null)}</> };
      default:
        return column;
    }
  });

  const actions = (row: InventoryMovementQueryRow) => (
    <>
      <Show when={props.onOpenInventoryMovementGet}>
        <Button type="button" variant="outline" size="sm" onClick={() => props.onOpenInventoryMovementGet?.(row)}>
          get
        </Button>
      </Show>
    </>
  );

  return (
    <TableScreen>
      <DataTable
        name="inventory-movement"
        columns={columns}
        rowId={INVENTORY_MOVEMENT_QUERY_TABLE.rowId}
        rows={load.state().rows}
        fullyRead={load.state().fullyRead}
        busy={load.state().busy}
        refusal={load.state().refusal}
        cap={load.state().cap}
        onCapChange={(cap) => void load.load(cap)}
        onRefresh={() => void load.load()}
        startedAt={load.state().startedAt}
        endedAt={load.state().endedAt}
        sortFields={INVENTORY_MOVEMENT_QUERY_TABLE.sortFields}
        sortMaxFields={INVENTORY_MOVEMENT_QUERY_TABLE.sortMaxFields}
        onSortChange={load.sortBy}
        scopeFilters={INVENTORY_MOVEMENT_QUERY_TABLE.scopeFilters}
        onScopeChange={() => void load.load()}
        rowActions={actions}
      />
    </TableScreen>
  );
}

/** The table definition of `wamn-wms:inventory-movement/query@1.0.0`. */
export const INVENTORY_MOVEMENT_QUERY_TABLE = {
  read: "query",
  rowId: ["id"],
  pageMaximum: 100,
  scopeFilters: [],
  sortFields: [],
  sortDirections: [],
  sortMaxFields: 1,
  columns: [
    { field: "createdAt", label: "created at", type: "timestamptz", role: "value" },
    { field: "createdBy", label: "created by", type: "uuid", role: "value" },
    { field: "fromLocationId", label: "from location id", type: "uuid", role: "reference", displayField: "locationCode" },
    { field: "id", label: "id", type: "uuid", role: "key" },
    { field: "idempotencyKey", label: "idempotency key", type: "text", role: "value" },
    { field: "kind", label: "kind", type: "text", role: "value" },
    { field: "occurredAt", label: "occurred at", type: "timestamptz", role: "value" },
    { field: "palletId", label: "pallet id", type: "uuid", role: "reference", displayField: "palletCode" },
    { field: "productId", label: "product id", type: "uuid", role: "reference", displayField: "productCode" },
    { field: "quantity", label: "quantity", type: "numeric", role: "value" },
    { field: "reasonCode", label: "reason code", type: "text", role: "value" },
    { field: "toLocationId", label: "to location id", type: "uuid", role: "reference", displayField: "locationCode" },
  ],
  actions: [
    { operation: "wamn-wms:inventory-movement/get@1.0.0", label: "get", many: false },
  ],
  childTables: [],
} as const;
