// @generated from the client-contract IR; do not edit.
//
// `inventory_transaction` components. Each one calls the bindings and the runtime, and
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
  DataGridTable,
  DetailItem,
  DetailList,
  FieldError,
  FieldGroup,
  FormActions,
  TableScreen,
  TextField,
  announceOutcome,
  createRecordLabels,
  gridFeatures,
  type GridFeatures,
} from "@wamn/ui";
import {
  get,
  query,
  type InventoryTransactionGetRequest,
  type InventoryTransactionGetResult,
  type InventoryTransactionQueryRequest,
  type InventoryTransactionQueryResult,
  type InventoryTransactionQueryRow,
} from "../inventory_transaction.js";
import {
  get as inventoryGet,
  type InventoryGetRequest,
} from "../inventory.js";
import {
  get as locationGet,
  type LocationGetRequest,
} from "../location.js";
import {
  get as packagingGet,
  type PackagingGetRequest,
} from "../packaging.js";
import {
  get as productGet,
  type ProductGetRequest,
} from "../product.js";

/** The record that the detail for `wamn-wms:inventory-transaction/get@1.0.0` reads. */
export interface InventoryTransactionGetDetailInput {
  readonly id: Uuid;
}

/** What the detail screen for `wamn-wms:inventory-transaction/get@1.0.0` takes. */
export interface InventoryTransactionGetDetailProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** The input that names the record. */
  readonly input: InventoryTransactionGetDetailInput;
  /** Called with every outcome this screen reads. */
  readonly onOutcome?: (outcome: Outcome<InventoryTransactionGetResult>) => void;
}

/** What an operator calls this screen. The page decides where it goes. */
export const InventoryTransactionGetDetailLabel = "get";

/**
 * The detail screen for `wamn-wms:inventory-transaction/get@1.0.0`.
 *
 * It reads when it mounts and again whenever its input changes, because the
 * input names the record it shows.
 */
export function InventoryTransactionGetDetail(props: InventoryTransactionGetDetailProps) {
  const [outcome] = createResource(
    () => props.input,
    async (input: InventoryTransactionGetDetailInput) => {
      const read = await get(props.transport, [
        { ...input } as InventoryTransactionGetRequest,
      ]);
      props.onOutcome?.(read);
      if (read.status !== "completed") {
        announceOutcome(read, InventoryTransactionGetDetailLabel);
      }
      return read;
    },
  );
  const record = (): InventoryTransactionGetResult | undefined => {
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
        <DetailItem term="from disposition">{cellText(readMember(record(), ["fromDisposition"]), "text")}</DetailItem>
        <DetailItem term="from inventory id">{cellText(readMember(record(), ["fromInventoryId"]), "uuid")}</DetailItem>
        <DetailItem term="from lifecycle">{cellText(readMember(record(), ["fromLifecycle"]), "text")}</DetailItem>
        <DetailItem term="from location id">{cellText(readMember(record(), ["fromLocationId"]), "uuid")}</DetailItem>
        <DetailItem term="from packaging id">{cellText(readMember(record(), ["fromPackagingId"]), "uuid")}</DetailItem>
        <DetailItem term="from product id">{cellText(readMember(record(), ["fromProductId"]), "uuid")}</DetailItem>
        <DetailItem term="from quantity">{cellText(readMember(record(), ["fromQuantity"]), "numeric")}</DetailItem>
        <DetailItem term="id">{cellText(readMember(record(), ["id"]), "uuid")}</DetailItem>
        <DetailItem term="inventory id">{cellText(readMember(record(), ["inventoryId"]), "uuid")}</DetailItem>
        <DetailItem term="occurred at">{cellText(readMember(record(), ["occurredAt"]), "timestamptz")}</DetailItem>
        <DetailItem term="operation id">{cellText(readMember(record(), ["operationId"]), "uuid")}</DetailItem>
        <DetailItem term="reason">{cellText(readMember(record(), ["reason"]), "text")}</DetailItem>
        <DetailItem term="to disposition">{cellText(readMember(record(), ["toDisposition"]), "text")}</DetailItem>
        <DetailItem term="to inventory id">{cellText(readMember(record(), ["toInventoryId"]), "uuid")}</DetailItem>
        <DetailItem term="to lifecycle">{cellText(readMember(record(), ["toLifecycle"]), "text")}</DetailItem>
        <DetailItem term="to location id">{cellText(readMember(record(), ["toLocationId"]), "uuid")}</DetailItem>
        <DetailItem term="to packaging id">{cellText(readMember(record(), ["toPackagingId"]), "uuid")}</DetailItem>
        <DetailItem term="to product id">{cellText(readMember(record(), ["toProductId"]), "uuid")}</DetailItem>
        <DetailItem term="to quantity">{cellText(readMember(record(), ["toQuantity"]), "numeric")}</DetailItem>
        <DetailItem term="type">{cellText(readMember(record(), ["type"]), "text")}</DetailItem>
      </DetailList>
    </section>
  );
}

/** What the table for `wamn-wms:inventory-transaction/query@1.0.0` takes. */
export interface InventoryTransactionQueryTableProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** Input the parent fixes, which the operator does not edit. */
  readonly fixed?: Partial<InventoryTransactionQueryRequest>;
  /** Called when the operator picks one row. */
  readonly onRowSelect?: (row: InventoryTransactionQueryRow) => void;
  /** Called when the operator opens `wamn-wms:inventory-transaction/get@1.0.0` from one row. */
  readonly onOpenInventoryTransactionGet?: (row: InventoryTransactionQueryRow) => void;
  /** Called with every outcome this screen reads. */
  readonly onOutcome?: (outcome: Outcome<InventoryTransactionQueryResult>) => void;
}

/** What an operator calls this screen. The page decides where it goes. */
export const InventoryTransactionQueryTableLabel = "query";

/**
 * The table for `wamn-wms:inventory-transaction/query@1.0.0`.
 *
 * It owns its page controls and its rows. A change to a control clears the
 * rows, because a cursor names a position in the list the old input produced.
 *
 * It reads when the operator asks, and not when it mounts, because a read is
 * a request that the operator did not send yet.
 */
export function InventoryTransactionQueryTable(props: InventoryTransactionQueryTableProps) {
  const [controls, setControls] = createSignal<Partial<InventoryTransactionQueryRequest>>({});
  const [page, setPage] = createSignal<PageState<InventoryTransactionQueryRow>>(emptyPage<InventoryTransactionQueryRow>());

  const read = async (cursor: string | null) => {
    setPage(startRead(page()));
    const request = {
      ...controls(),
      ...props.fixed,
    } as InventoryTransactionQueryRequest;
    const sent = cursor === null ? request : (writeMember(request, ["cursor"], cursor) as InventoryTransactionQueryRequest);
    const outcome = await query(props.transport, [sent]);
    props.onOutcome?.(outcome);
    if (outcome.status !== "completed") {
      announceOutcome(outcome, InventoryTransactionQueryTableLabel);
      setPage(failedRead(page(), outcome));
      return;
    }
    const rows = outcome.value.item;
    setPage(cursor === null ? firstPage(rows, outcome.value.nextCursor) : appendPage(page(), rows, outcome.value.nextCursor));
  };

  const restart = () => {
    setPage(emptyPage<InventoryTransactionQueryRow>());
    void read(null);
  };

  const change = (path: readonly string[], value: JsonValue) => {
    setControls((current) => writeControl(current, path, value));
    restart();
  };

  const inventoryGetLabels = createRecordLabels(async (key) => {
    const request = writeMember({}, ["id"], key) as InventoryGetRequest;
    const outcome = await inventoryGet(props.transport, [request]);
    if (outcome.status !== "completed") {
      return null;
    }
    const text = outcome.value.disposition;
    return text == null ? null : String(text);
  });
  const locationGetLabels = createRecordLabels(async (key) => {
    const request = writeMember({}, ["id"], key) as LocationGetRequest;
    const outcome = await locationGet(props.transport, [request]);
    if (outcome.status !== "completed") {
      return null;
    }
    const text = outcome.value.locationCode;
    return text == null ? null : String(text);
  });
  const packagingGetLabels = createRecordLabels(async (key) => {
    const request = writeMember({}, ["id"], key) as PackagingGetRequest;
    const outcome = await packagingGet(props.transport, [request]);
    if (outcome.status !== "completed") {
      return null;
    }
    const text = outcome.value.code;
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

  const columns: ColumnDef<GridFeatures, InventoryTransactionQueryRow>[] = [
    {
      accessorKey: "createdAt",
      header: "created at",
      cell: (cell) => cellText(cell.getValue() as JsonValue, "timestamptz"),
    },
    {
      accessorKey: "fromDisposition",
      header: "from disposition",
      cell: (cell) => cellText(cell.getValue() as JsonValue, "text"),
    },
    {
      accessorKey: "fromInventoryId",
      header: "from inventory id",
      cell: (cell) => <>{inventoryGetLabels(cell.getValue() as string | null)}</>,
    },
    {
      accessorKey: "fromLifecycle",
      header: "from lifecycle",
      cell: (cell) => cellText(cell.getValue() as JsonValue, "text"),
    },
    {
      accessorKey: "fromLocationId",
      header: "from location id",
      cell: (cell) => <>{locationGetLabels(cell.getValue() as string | null)}</>,
    },
    {
      accessorKey: "fromPackagingId",
      header: "from packaging id",
      cell: (cell) => <>{packagingGetLabels(cell.getValue() as string | null)}</>,
    },
    {
      accessorKey: "fromProductId",
      header: "from product id",
      cell: (cell) => <>{productGetLabels(cell.getValue() as string | null)}</>,
    },
    {
      accessorKey: "fromQuantity",
      header: "from quantity",
      cell: (cell) => cellText(cell.getValue() as JsonValue, "numeric"),
    },
    {
      accessorKey: "id",
      header: "id",
      cell: (cell) => cellText(cell.getValue() as JsonValue, "uuid"),
    },
    {
      accessorKey: "inventoryId",
      header: "inventory id",
      cell: (cell) => <>{inventoryGetLabels(cell.getValue() as string | null)}</>,
    },
    {
      accessorKey: "occurredAt",
      header: "occurred at",
      cell: (cell) => cellText(cell.getValue() as JsonValue, "timestamptz"),
    },
    {
      accessorKey: "operationId",
      header: "operation id",
      cell: (cell) => cellText(cell.getValue() as JsonValue, "uuid"),
    },
    {
      accessorKey: "reason",
      header: "reason",
      cell: (cell) => cellText(cell.getValue() as JsonValue, "text"),
    },
    {
      accessorKey: "toDisposition",
      header: "to disposition",
      cell: (cell) => cellText(cell.getValue() as JsonValue, "text"),
    },
    {
      accessorKey: "toInventoryId",
      header: "to inventory id",
      cell: (cell) => <>{inventoryGetLabels(cell.getValue() as string | null)}</>,
    },
    {
      accessorKey: "toLifecycle",
      header: "to lifecycle",
      cell: (cell) => cellText(cell.getValue() as JsonValue, "text"),
    },
    {
      accessorKey: "toLocationId",
      header: "to location id",
      cell: (cell) => <>{locationGetLabels(cell.getValue() as string | null)}</>,
    },
    {
      accessorKey: "toPackagingId",
      header: "to packaging id",
      cell: (cell) => <>{packagingGetLabels(cell.getValue() as string | null)}</>,
    },
    {
      accessorKey: "toProductId",
      header: "to product id",
      cell: (cell) => <>{productGetLabels(cell.getValue() as string | null)}</>,
    },
    {
      accessorKey: "toQuantity",
      header: "to quantity",
      cell: (cell) => cellText(cell.getValue() as JsonValue, "numeric"),
    },
    {
      accessorKey: "type",
      header: "type",
      cell: (cell) => cellText(cell.getValue() as JsonValue, "text"),
    },
    {
      id: "openInventoryTransactionGet",
      header: "",
      cell: (cell) => (
        <Show when={props.onOpenInventoryTransactionGet}>
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => props.onOpenInventoryTransactionGet?.(cell.row.original)}
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
      return page().rows as InventoryTransactionQueryRow[];
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
          <DataGridTable />
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
