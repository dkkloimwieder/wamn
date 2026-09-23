// @generated from the client-contract IR; do not edit.
//
// `location` components. Each one calls the bindings and the runtime, and
// nothing else.

import { Show, createSignal } from "solid-js";
import { createTable, type ColumnDef } from "@tanstack/solid-table";
import {
  appendPage,
  cellText,
  emptyPage,
  firstPage,
  newRequestId,
  startRead,
  type JsonValue,
  type Outcome,
  type PageState,
  type Transport,
  writeMember,
} from "@wamn/web-runtime";
import {
  Button,
  DataGrid,
  DataGridContainer,
  DataGridTable,
  FormActions,
  TableScreen,
  announceOutcome,
  gridFeatures,
  type GridFeatures,
} from "@wamn/ui";
import {
  list,
  type LocationListRequest,
  type LocationListResult,
  type LocationListRow,
} from "../location.js";
import {
  type ReceivingRecordReceiptFormInitial,
} from "./receiving.js";

/** Columns of `wamn-receiving:location/list@1.0.0`, in contract order. */
const LIST_COLUMNS: ColumnDef<GridFeatures, LocationListRow>[] = [
  {
    accessorKey: "id",
    header: "id",
    cell: (cell) => cellText(cell.getValue() as JsonValue, "uuid"),
  },
  {
    accessorKey: "locationCode",
    header: "Location code",
    cell: (cell) => cellText(cell.getValue() as JsonValue, "text"),
  },
];

/** What the table for `wamn-receiving:location/list@1.0.0` takes. */
export interface LocationListTableProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** Input the parent fixes, which the operator does not edit. */
  readonly fixed?: Partial<LocationListRequest>;
  /** Called when the operator picks one row. */
  readonly onRowSelect?: (row: LocationListRow) => void;
  /** Called with the values one row hands to `wamn-receiving:receiving/record-receipt@1.0.0`. */
  readonly onFillReceivingRecordReceipt?: (initial: ReceivingRecordReceiptFormInitial) => void;
  /** Called with every outcome this screen reads. */
  readonly onOutcome?: (outcome: Outcome<LocationListResult>) => void;
}

/** What an operator calls this screen. The page decides where it goes. */
export const LocationListTableLabel = "Locations";

/**
 * The table for `wamn-receiving:location/list@1.0.0`.
 *
 * It owns its page controls and its rows. A change to a control clears the
 * rows, because a cursor names a position in the list the old input produced.
 *
 * It reads when the operator asks, and not when it mounts, because a read is
 * a request that the operator did not send yet.
 */
export function LocationListTable(props: LocationListTableProps) {
  const controls = (): Partial<LocationListRequest> => ({});
  const [page, setPage] = createSignal<PageState<LocationListRow>>(emptyPage<LocationListRow>());

  const read = async (cursor: string | null) => {
    setPage(startRead(page()));
    const request = {
      ...controls(),
      ...props.fixed,
      requestId: newRequestId(),
    } as LocationListRequest;
    const sent = request;
    const outcome = await list(props.transport, [sent]);
    props.onOutcome?.(outcome);
    if (outcome.status !== "completed") {
      announceOutcome(outcome, LocationListTableLabel);
      setPage({ ...page(), busy: false });
      return;
    }
    const rows = outcome.value.rows;
    setPage(cursor === null ? firstPage(rows, null) : appendPage(page(), rows, null));
  };

  const restart = () => {
    setPage(emptyPage<LocationListRow>());
    void read(null);
  };

  const columns: ColumnDef<GridFeatures, LocationListRow>[] = [
    ...LIST_COLUMNS,
    {
      id: "fillReceivingRecordReceipt",
      header: "",
      cell: (cell) => (
        <Show when={props.onFillReceivingRecordReceipt}>
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => props.onFillReceivingRecordReceipt?.(writeMember({} as ReceivingRecordReceiptFormInitial, ["value", "line", "locationId"], cell.row.original.id))}
          >
            record-receipt
          </Button>
        </Show>
      ),
    },
  ];

  const table = createTable({
    features: gridFeatures,
    get data() {
      return page().rows as LocationListRow[];
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
        <FormActions>
          <Button type="submit">read</Button>
        </FormActions>
      </form>
      <DataGrid
        table={table}
        recordCount={page().rows.length}
        isLoading={page().busy && page().rows.length === 0}
        onRowClick={(row) => props.onRowSelect?.(row)}
      >
        <DataGridContainer>
          <DataGridTable />
        </DataGridContainer>
      </DataGrid>
    </TableScreen>
  );
}
