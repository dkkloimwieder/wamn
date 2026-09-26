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
  type PalletQuantityGetRequest,
  type PalletQuantityGetResult,
  type PalletQuantityQueryRequest,
  type PalletQuantityQueryResult,
  type PalletQuantityQueryRow,
} from "../pallet_quantity.js";
import {
  get as palletGet,
  type PalletGetRequest,
} from "../pallet.js";
import {
  get as productGet,
  type ProductGetRequest,
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
  /** Called when the operator opens `wamn-wms:pallet-quantity/get@1.0.0` from one row. */
  readonly onOpenPalletQuantityGet?: (row: PalletQuantityQueryRow) => void;
  /** Called with every outcome this screen reads. */
  readonly onOutcome?: (outcome: Outcome<PalletQuantityQueryResult>) => void;
}

/** What an operator calls this screen. The page decides where it goes. */
export const PalletQuantityQueryTableLabel = "query";

/**
 * The table for `wamn-wms:pallet-quantity/query@1.0.0`: the DataTable over `PALLET_QUANTITY_QUERY_TABLE`.
 *
 * It loads when it mounts. A change to a filter, a sort of rows the load did
 * not read in full, a cap change and a refresh each start a new load.
 */
export function PalletQuantityQueryTable(props: PalletQuantityQueryTableProps) {
  const load = createTableLoad<PalletQuantityQueryRow>(PALLET_QUANTITY_QUERY_TABLE, async (limit) => {
    let request = { ...props.fixed } as PalletQuantityQueryRequest;
    request = writeMember(request, ["limit"], limit) as PalletQuantityQueryRequest;
    const outcome = await query(props.transport, [request]);
    props.onOutcome?.(outcome);
    if (outcome.status !== "completed") {
      announceOutcome(outcome, PalletQuantityQueryTableLabel);
    }
    return outcome;
  });
  void load.load();
  onCleanup(afterWrites(props.transport, () => void load.load()));

  const palletGetLabels = createRecordLabels(props.transport, async (key) => {
    const request = writeMember({}, ["id"], key) as PalletGetRequest;
    const outcome = await palletGet(props.transport, [request]);
    if (outcome.status !== "completed") {
      return null;
    }
    const text = outcome.value.palletCode;
    return text == null ? null : String(text);
  });
  const productGetLabels = createRecordLabels(props.transport, async (key) => {
    const request = writeMember({}, ["id"], key) as ProductGetRequest;
    const outcome = await productGet(props.transport, [request]);
    if (outcome.status !== "completed") {
      return null;
    }
    const text = outcome.value.productCode;
    return text == null ? null : String(text);
  });

  const columns = PALLET_QUANTITY_QUERY_TABLE.columns.map((column) => {
    switch (column.field) {
      case "palletId":
        return { ...column, cell: (value: unknown) => <>{palletGetLabels(value as string | null)}</> };
      case "productId":
        return { ...column, cell: (value: unknown) => <>{productGetLabels(value as string | null)}</> };
      default:
        return column;
    }
  });

  const actions = (row: PalletQuantityQueryRow) => (
    <>
      <Show when={props.onOpenPalletQuantityGet}>
        <Button type="button" variant="outline" size="sm" onClick={() => props.onOpenPalletQuantityGet?.(row)}>
          get
        </Button>
      </Show>
    </>
  );

  return (
    <TableScreen>
      <DataTable
        name="pallet-quantity"
        columns={columns}
        rowId={PALLET_QUANTITY_QUERY_TABLE.rowId}
        rows={load.state().rows}
        fullyRead={load.state().fullyRead}
        busy={load.state().busy}
        refusal={load.state().refusal}
        cap={load.state().cap}
        onCapChange={(cap) => void load.load(cap)}
        onRefresh={() => void load.load()}
        startedAt={load.state().startedAt}
        endedAt={load.state().endedAt}
        sortFields={PALLET_QUANTITY_QUERY_TABLE.sortFields}
        sortMaxFields={PALLET_QUANTITY_QUERY_TABLE.sortMaxFields}
        onSortChange={load.sortBy}
        scopeFilters={PALLET_QUANTITY_QUERY_TABLE.scopeFilters}
        onScopeChange={() => void load.load()}
        rowActions={actions}
      />
    </TableScreen>
  );
}

/** The table definition of `wamn-wms:pallet-quantity/query@1.0.0`. */
export const PALLET_QUANTITY_QUERY_TABLE = {
  read: "query",
  rowId: ["id"],
  pageMaximum: 100,
  scopeFilters: [],
  sortFields: [],
  sortDirections: [],
  sortMaxFields: 1,
  columns: [
    { field: "createdAt", label: "created at", type: "timestamptz", role: "value" },
    { field: "id", label: "id", type: "uuid", role: "key" },
    { field: "palletId", label: "pallet id", type: "uuid", role: "reference", displayField: "palletCode" },
    { field: "productId", label: "product id", type: "uuid", role: "reference", displayField: "productCode" },
    { field: "quantity", label: "quantity", type: "numeric", role: "value" },
    { field: "status", label: "status", type: "text", role: "value" },
  ],
  actions: [
    { operation: "wamn-wms:pallet-quantity/get@1.0.0", label: "get", many: false },
  ],
  childTables: [],
} as const;
