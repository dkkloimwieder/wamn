// @generated from the client-contract IR; do not edit.
//
// `pallet_quantity` components. Each one calls the bindings and the runtime, and
// nothing else.

import { Show, createResource, createSignal } from "solid-js";
import { createTable, type ColumnDef } from "@tanstack/solid-table";
import {
  appendPage,
  cellText,
  emptyPage,
  failedRead,
  firstPage,
  hasNextPage,
  readMember,
  startRead,
  type JsonValue,
  type Outcome,
  type PageState,
  type Transport,
  type Uuid,
  writeControl,
  writeMember,
} from "@wamn/web-runtime";
import {
  Button,
  DataGrid,
  DataGridContainer,
  DetailItem,
  DetailList,
  FieldError,
  FieldGroup,
  FormActions,
  TableScreen,
  TextField,
  WindowedTable,
  announceOutcome,
  createRecordLabels,
  gridFeatures,
  type GridFeatures,
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
  const [outcome] = createResource(
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
  /** Called when the operator picks one row. */
  readonly onRowSelect?: (row: PalletQuantityQueryRow) => void;
  /** Called when the operator opens `wamn-wms:pallet-quantity/get@1.0.0` from one row. */
  readonly onOpenPalletQuantityGet?: (row: PalletQuantityQueryRow) => void;
  /** Called with every outcome this screen reads. */
  readonly onOutcome?: (outcome: Outcome<PalletQuantityQueryResult>) => void;
}

/** What an operator calls this screen. The page decides where it goes. */
export const PalletQuantityQueryTableLabel = "query";

/**
 * The table for `wamn-wms:pallet-quantity/query@1.0.0`.
 *
 * It owns its page controls and its rows. A change to a control clears the
 * rows, because a cursor names a position in the list the old input produced.
 *
 * It reads when the operator asks, and not when it mounts, because a read is
 * a request that the operator did not send yet.
 */
export function PalletQuantityQueryTable(props: PalletQuantityQueryTableProps) {
  const [controls, setControls] = createSignal<Partial<PalletQuantityQueryRequest>>({});
  const [page, setPage] = createSignal<PageState<PalletQuantityQueryRow>>(emptyPage<PalletQuantityQueryRow>());

  const read = async (cursor: string | null) => {
    setPage(startRead(page()));
    const request = {
      ...controls(),
      ...props.fixed,
    } as PalletQuantityQueryRequest;
    const sent = cursor === null ? request : (writeMember(request, ["cursor"], cursor) as PalletQuantityQueryRequest);
    const outcome = await query(props.transport, [sent]);
    props.onOutcome?.(outcome);
    if (outcome.status !== "completed") {
      announceOutcome(outcome, PalletQuantityQueryTableLabel);
      setPage(failedRead(page(), outcome));
      return;
    }
    const rows = outcome.value.item;
    setPage(cursor === null ? firstPage(rows, outcome.value.nextCursor) : appendPage(page(), rows, outcome.value.nextCursor));
  };

  const restart = () => {
    setPage(emptyPage<PalletQuantityQueryRow>());
    void read(null);
  };

  const change = (path: readonly string[], value: JsonValue) => {
    setControls((current) => writeControl(current, path, value));
    restart();
  };

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

  const columns: ColumnDef<GridFeatures, PalletQuantityQueryRow>[] = [
    {
      accessorKey: "createdAt",
      header: "created at",
      cell: (cell) => cellText(cell.getValue() as JsonValue, "timestamptz"),
    },
    {
      accessorKey: "id",
      header: "id",
      cell: (cell) => cellText(cell.getValue() as JsonValue, "uuid"),
    },
    {
      accessorKey: "palletId",
      header: "pallet id",
      cell: (cell) => <>{palletGetLabels(cell.getValue() as string | null)}</>,
    },
    {
      accessorKey: "productId",
      header: "product id",
      cell: (cell) => <>{productGetLabels(cell.getValue() as string | null)}</>,
    },
    {
      accessorKey: "quantity",
      header: "quantity",
      cell: (cell) => cellText(cell.getValue() as JsonValue, "numeric"),
    },
    {
      accessorKey: "status",
      header: "status",
      cell: (cell) => cellText(cell.getValue() as JsonValue, "text"),
    },
    {
      id: "openPalletQuantityGet",
      header: "",
      cell: (cell) => (
        <Show when={props.onOpenPalletQuantityGet}>
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => props.onOpenPalletQuantityGet?.(cell.row.original)}
          >
            get
          </Button>
        </Show>
      ),
    },
  ];

  const table = createTable({
    features: gridFeatures,
    get data() {
      return page().rows as PalletQuantityQueryRow[];
    },
    columns: columns,
    manualPagination: true,
  });

  return (
    <TableScreen>
      <form
        onSubmit={(event) => {
          event.preventDefault();
          restart();
        }}
      >
        <FieldGroup>
          <TextField
            label="limit"
            type="number"
            min={1}
            max={100}
            value="100"
            onChange={(value) => change(["limit"], value)}
          />
        </FieldGroup>
        <FormActions>
          <Button type="submit">read</Button>
        </FormActions>
      </form>
      <DataGrid
        table={table}
        recordCount={page().rows.length}
        isLoading={page().busy && page().rows.length === 0}
        emptyMessage={page().refusal}
        onRowClick={(row) => props.onRowSelect?.(row)}
      >
        <DataGridContainer>
          <WindowedTable />
        </DataGridContainer>
      </DataGrid>
      <FormActions>
        <Button
          type="button"
          variant="outline"
          disabled={!hasNextPage(page())}
          onClick={() => void read(page().cursor)}
        >
          next page
        </Button>
      </FormActions>
    </TableScreen>
  );
}

/** The table definition of `wamn-wms:pallet-quantity/query@1.0.0`. */
export const PALLET_QUANTITY_QUERY_TABLE = {
  read: "query",
  rowId: "id",
  pageMaximum: 100,
  scopeFilters: [],
  sortFields: [],
  sortDirections: [],
  columns: [
    { field: "createdAt", label: "created at", type: "timestamptz" },
    { field: "id", label: "id", type: "uuid" },
    { field: "palletId", label: "pallet id", type: "uuid", displayField: "palletCode" },
    { field: "productId", label: "product id", type: "uuid", displayField: "productCode" },
    { field: "quantity", label: "quantity", type: "numeric" },
    { field: "status", label: "status", type: "text" },
  ],
} as const;
