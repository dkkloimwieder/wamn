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
} from "@wamn/web-runtime";
import {
  DetailItem,
  DetailList,
  FieldError,
  QueryTable,
  TableScreen,
  announceOutcome,
} from "@wamn/ui";
import {
  INVENTORY_MOVEMENT_QUERY_REQUEST_FIELDS,
  INVENTORY_MOVEMENT_QUERY_RESULT_FIELDS,
  INVENTORY_MOVEMENT_QUERY_ROUTE,
  get,
  type InventoryMovementGetRequest,
  type InventoryMovementGetResult,
  type InventoryMovementQueryRequest,
  type InventoryMovementQueryResult,
  type InventoryMovementQueryRow,
} from "../inventory_movement.js";
import {
  LOCATION_GET_REQUEST_FIELDS,
  LOCATION_GET_RESULT_FIELDS,
  LOCATION_GET_ROUTE,
} from "../location.js";
import {
  PALLET_GET_REQUEST_FIELDS,
  PALLET_GET_RESULT_FIELDS,
  PALLET_GET_ROUTE,
} from "../pallet.js";
import {
  PRODUCT_GET_REQUEST_FIELDS,
  PRODUCT_GET_RESULT_FIELDS,
  PRODUCT_GET_ROUTE,
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
  /** For each operation whose record a row opens, what opening it does. A row shows a button only for these. */
  readonly onOpen?: { readonly [operation: string]: (row: InventoryMovementQueryRow) => void };
  /** For each operation whose form a row fills, what the filled values do. */
  readonly onFill?: { readonly [operation: string]: (initial: object) => void };
  /** Called with every outcome this screen reads. */
  readonly onOutcome?: (outcome: Outcome<InventoryMovementQueryResult>) => void;
}

/** What an operator calls this screen. The page decides where it goes. */
export const InventoryMovementQueryTableLabel = "query";

/** The table for `wamn-wms:inventory-movement/query@1.0.0`: the QueryTable over `INVENTORY_MOVEMENT_QUERY_TABLE`, in the table screen. */
export function InventoryMovementQueryTable(props: InventoryMovementQueryTableProps) {
  return (
    <TableScreen>
      <QueryTable<InventoryMovementQueryRow, InventoryMovementQueryResult> definition={INVENTORY_MOVEMENT_QUERY_TABLE} label={InventoryMovementQueryTableLabel} {...props} />
    </TableScreen>
  );
}

/** The table definition of `wamn-wms:inventory-movement/query@1.0.0`. */
export const INVENTORY_MOVEMENT_QUERY_TABLE = {
  name: "inventory-movement",
  read: { route: INVENTORY_MOVEMENT_QUERY_ROUTE, request: INVENTORY_MOVEMENT_QUERY_REQUEST_FIELDS, result: INVENTORY_MOVEMENT_QUERY_RESULT_FIELDS },
  rows: "item",
  rowId: ["id"],
  pageMaximum: 100,
  limitInput: ["limit"],
  sortFieldInput: null,
  sortDirectionInput: null,
  filters: [],
  scopeFilters: [],
  sortFields: [],
  sortDirections: [],
  sortMaxFields: 1,
  columns: [
    { field: "createdAt", label: "created at", type: "timestamptz", role: "value" },
    { field: "createdBy", label: "created by", type: "uuid", role: "value" },
    { field: "fromLocationId", label: "from location id", type: "uuid", role: "reference", displayField: "locationCode", recordRead: { read: { route: LOCATION_GET_ROUTE, request: LOCATION_GET_REQUEST_FIELDS, result: LOCATION_GET_RESULT_FIELDS }, keyInput: ["id"] } },
    { field: "id", label: "id", type: "uuid", role: "key" },
    { field: "idempotencyKey", label: "idempotency key", type: "text", role: "value" },
    { field: "kind", label: "kind", type: "text", role: "value" },
    { field: "occurredAt", label: "occurred at", type: "timestamptz", role: "value" },
    { field: "palletId", label: "pallet id", type: "uuid", role: "reference", displayField: "palletCode", recordRead: { read: { route: PALLET_GET_ROUTE, request: PALLET_GET_REQUEST_FIELDS, result: PALLET_GET_RESULT_FIELDS }, keyInput: ["id"] } },
    { field: "productId", label: "product id", type: "uuid", role: "reference", displayField: "productCode", recordRead: { read: { route: PRODUCT_GET_ROUTE, request: PRODUCT_GET_REQUEST_FIELDS, result: PRODUCT_GET_RESULT_FIELDS }, keyInput: ["id"] } },
    { field: "quantity", label: "quantity", type: "numeric", role: "value" },
    { field: "reasonCode", label: "reason code", type: "text", role: "value" },
    { field: "toLocationId", label: "to location id", type: "uuid", role: "reference", displayField: "locationCode", recordRead: { read: { route: LOCATION_GET_ROUTE, request: LOCATION_GET_REQUEST_FIELDS, result: LOCATION_GET_RESULT_FIELDS }, keyInput: ["id"] } },
  ],
  actions: [
    { operation: "wamn-wms:inventory-movement/get@1.0.0", label: "get", many: false, opens: "record", fill: [] },
  ],
  childTables: [],
} as const;
