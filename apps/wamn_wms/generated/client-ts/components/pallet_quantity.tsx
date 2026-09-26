// @generated from the client-contract IR; do not edit.
//
// `pallet_quantity` components. Each one calls the bindings and the runtime, and
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
  announceOutcome,
} from "@wamn/ui";
import {
  PALLET_QUANTITY_QUERY_REQUEST_FIELDS,
  PALLET_QUANTITY_QUERY_RESULT_FIELDS,
  PALLET_QUANTITY_QUERY_ROUTE,
  get,
  type PalletQuantityGetRequest,
  type PalletQuantityGetResult,
  type PalletQuantityQueryRequest,
  type PalletQuantityQueryResult,
  type PalletQuantityQueryRow,
} from "../pallet_quantity.js";
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

/** The record that the detail for `wamn-wms:pallet-quantity/get@1.0.0` reads. */
export interface PalletQuantityGetDetailInput {
  readonly id: Uuid;
}

/** What the detail screen for `wamn-wms:pallet-quantity/get@1.0.0` takes. */
export interface PalletQuantityGetDetailProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** The input that names the record. */
  readonly input: PalletQuantityGetDetailInput;
  /** Called with every outcome this screen reads. */
  readonly onOutcome?: (outcome: Outcome<PalletQuantityGetResult>) => void;
}

/** What an operator calls this screen. The page decides where it goes. */
export const PalletQuantityGetDetailLabel = "get";

/**
 * The detail screen for `wamn-wms:pallet-quantity/get@1.0.0`.
 *
 * It reads when it mounts and again whenever its input changes, because the
 * input names the record it shows.
 */
export function PalletQuantityGetDetail(props: PalletQuantityGetDetailProps) {
  const [outcome, { refetch: readAgain }] = createResource(
    () => props.input,
    async (input: PalletQuantityGetDetailInput) => {
      const read = await get(props.transport, [
        input as PalletQuantityGetRequest,
      ]);
      props.onOutcome?.(read);
      if (read.status !== "completed") {
        announceOutcome(read, PalletQuantityGetDetailLabel);
      }
      return read;
    },
  );
  onCleanup(afterWrites(props.transport, () => void readAgain()));
  const record = (): PalletQuantityGetResult | undefined => {
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
        <DetailItem term="id">{cellText(readMember(record(), ["id"]), "uuid")}</DetailItem>
        <DetailItem term="pallet id">{cellText(readMember(record(), ["palletId"]), "uuid")}</DetailItem>
        <DetailItem term="product id">{cellText(readMember(record(), ["productId"]), "uuid")}</DetailItem>
        <DetailItem term="quantity">{cellText(readMember(record(), ["quantity"]), "numeric")}</DetailItem>
        <DetailItem term="status">{cellText(readMember(record(), ["status"]), "text")}</DetailItem>
      </DetailList>
    </section>
  );
}

/** What the table for `wamn-wms:pallet-quantity/query@1.0.0` takes. */
export interface PalletQuantityQueryTableProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** Input the parent fixes, which the operator does not edit. */
  readonly fixed?: Partial<PalletQuantityQueryRequest>;
  /** For each operation whose record a row opens, what opening it does. A row shows a button only for these. */
  readonly onOpen?: { readonly [operation: string]: (row: PalletQuantityQueryRow) => void };
  /** For each operation whose form a row fills, what the filled values do. */
  readonly onFill?: { readonly [operation: string]: (initial: object) => void };
  /** Called with every outcome this screen reads. */
  readonly onOutcome?: (outcome: Outcome<PalletQuantityQueryResult>) => void;
}

/** What an operator calls this screen. The page decides where it goes. */
export const PalletQuantityQueryTableLabel = "query";

/** The table for `wamn-wms:pallet-quantity/query@1.0.0`: the QueryTable over `PALLET_QUANTITY_QUERY_TABLE`. */
export function PalletQuantityQueryTable(props: PalletQuantityQueryTableProps) {
  return <QueryTable<PalletQuantityQueryRow, PalletQuantityQueryResult> definition={PALLET_QUANTITY_QUERY_TABLE} label={PalletQuantityQueryTableLabel} {...props} />;
}

/** The table definition of `wamn-wms:pallet-quantity/query@1.0.0`. */
export const PALLET_QUANTITY_QUERY_TABLE = {
  name: "pallet-quantity",
  read: { route: PALLET_QUANTITY_QUERY_ROUTE, request: PALLET_QUANTITY_QUERY_REQUEST_FIELDS, result: PALLET_QUANTITY_QUERY_RESULT_FIELDS },
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
    { field: "id", label: "id", type: "uuid", role: "key" },
    { field: "palletId", label: "pallet id", type: "uuid", role: "reference", displayField: "palletCode", recordRead: { read: { route: PALLET_GET_ROUTE, request: PALLET_GET_REQUEST_FIELDS, result: PALLET_GET_RESULT_FIELDS }, keyInput: ["id"] } },
    { field: "productId", label: "product id", type: "uuid", role: "reference", displayField: "productCode", recordRead: { read: { route: PRODUCT_GET_ROUTE, request: PRODUCT_GET_REQUEST_FIELDS, result: PRODUCT_GET_RESULT_FIELDS }, keyInput: ["id"] } },
    { field: "quantity", label: "quantity", type: "numeric", role: "value" },
    { field: "status", label: "status", type: "text", role: "value" },
  ],
  actions: [
    { operation: "wamn-wms:pallet-quantity/get@1.0.0", label: "get", many: false, opens: "record", fill: [] },
  ],
  childTables: [],
} as const;
