// @generated from the client-contract IR; do not edit.
//
// `inventory_movement` components. Each one calls the bindings and the runtime, and
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
  newRequestId,
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
  const [outcome] = createResource(
    () => props.input,
    async (input: InventoryMovementGetDetailInput) => {
      const read = await get(props.transport, [
        { ...input, requestId: newRequestId() } as InventoryMovementGetRequest,
      ]);
      props.onOutcome?.(read);
      if (read.status !== "completed") {
        announceOutcome(read, InventoryMovementGetDetailLabel);
      }
      return read;
    },
  );
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
  /** Called when the operator picks one row. */
  readonly onRowSelect?: (row: InventoryMovementQueryRow) => void;
  /** Called when the operator opens `wamn-wms:inventory-movement/get@1.0.0` from one row. */
  readonly onOpenInventoryMovementGet?: (row: InventoryMovementQueryRow) => void;
  /** Called with every outcome this screen reads. */
  readonly onOutcome?: (outcome: Outcome<InventoryMovementQueryResult>) => void;
}

/** What an operator calls this screen. The page decides where it goes. */
export const InventoryMovementQueryTableLabel = "query";

/**
 * The table for `wamn-wms:inventory-movement/query@1.0.0`.
 *
 * It owns its page controls and its rows. A change to a control clears the
 * rows, because a cursor names a position in the list the old input produced.
 *
 * It reads when the operator asks, and not when it mounts, because a read is
 * a request that the operator did not send yet.
 */
export function InventoryMovementQueryTable(props: InventoryMovementQueryTableProps) {
  const [controls, setControls] = createSignal<Partial<InventoryMovementQueryRequest>>({});
  const [page, setPage] = createSignal<PageState<InventoryMovementQueryRow>>(emptyPage<InventoryMovementQueryRow>());

  const read = async (cursor: string | null) => {
    setPage(startRead(page()));
    const request = {
      ...controls(),
      ...props.fixed,
      requestId: newRequestId(),
    } as InventoryMovementQueryRequest;
    const sent = cursor === null ? request : (writeMember(request, ["cursor"], cursor) as InventoryMovementQueryRequest);
    const outcome = await query(props.transport, [sent]);
    props.onOutcome?.(outcome);
    if (outcome.status !== "completed") {
      announceOutcome(outcome, InventoryMovementQueryTableLabel);
      setPage(failedRead(page(), outcome));
      return;
    }
    const rows = outcome.value.item;
    setPage(cursor === null ? firstPage(rows, outcome.value.nextCursor) : appendPage(page(), rows, outcome.value.nextCursor));
  };

  const restart = () => {
    setPage(emptyPage<InventoryMovementQueryRow>());
    void read(null);
  };

  const change = (path: readonly string[], value: JsonValue) => {
    setControls((current) => writeControl(current, path, value));
    restart();
  };

  const locationGetLabels = createRecordLabels(async (key) => {
    const request = writeMember({ requestId: newRequestId() }, ["id"], key) as LocationGetRequest;
    const outcome = await locationGet(props.transport, [request]);
    if (outcome.status !== "completed") {
      return null;
    }
    const text = outcome.value.locationCode;
    return text == null ? null : String(text);
  });
  const palletGetLabels = createRecordLabels(async (key) => {
    const request = writeMember({ requestId: newRequestId() }, ["id"], key) as PalletGetRequest;
    const outcome = await palletGet(props.transport, [request]);
    if (outcome.status !== "completed") {
      return null;
    }
    const text = outcome.value.palletCode;
    return text == null ? null : String(text);
  });
  const productGetLabels = createRecordLabels(async (key) => {
    const request = writeMember({ requestId: newRequestId() }, ["id"], key) as ProductGetRequest;
    const outcome = await productGet(props.transport, [request]);
    if (outcome.status !== "completed") {
      return null;
    }
    const text = outcome.value.productCode;
    return text == null ? null : String(text);
  });

  const columns: ColumnDef<GridFeatures, InventoryMovementQueryRow>[] = [
    {
      accessorKey: "createdAt",
      header: "created at",
      cell: (cell) => cellText(cell.getValue() as JsonValue, "timestamptz"),
    },
    {
      accessorKey: "createdBy",
      header: "created by",
      cell: (cell) => cellText(cell.getValue() as JsonValue, "uuid"),
    },
    {
      accessorKey: "fromLocationId",
      header: "from location id",
      cell: (cell) => <>{locationGetLabels(cell.getValue() as string | null)}</>,
    },
    {
      accessorKey: "id",
      header: "id",
      cell: (cell) => cellText(cell.getValue() as JsonValue, "uuid"),
    },
    {
      accessorKey: "idempotencyKey",
      header: "idempotency key",
      cell: (cell) => cellText(cell.getValue() as JsonValue, "text"),
    },
    {
      accessorKey: "kind",
      header: "kind",
      cell: (cell) => cellText(cell.getValue() as JsonValue, "text"),
    },
    {
      accessorKey: "occurredAt",
      header: "occurred at",
      cell: (cell) => cellText(cell.getValue() as JsonValue, "timestamptz"),
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
      accessorKey: "reasonCode",
      header: "reason code",
      cell: (cell) => cellText(cell.getValue() as JsonValue, "text"),
    },
    {
      accessorKey: "toLocationId",
      header: "to location id",
      cell: (cell) => <>{locationGetLabels(cell.getValue() as string | null)}</>,
    },
    {
      id: "openInventoryMovementGet",
      header: "",
      cell: (cell) => (
        <Show when={props.onOpenInventoryMovementGet}>
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => props.onOpenInventoryMovementGet?.(cell.row.original)}
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
      return page().rows as InventoryMovementQueryRow[];
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
