// @generated from the client-contract IR; do not edit.
//
// `location` components. Each one calls the bindings and the runtime, and
// nothing else.

import { Show, onCleanup } from "solid-js";
import {
  afterWrites,
  boundedPage,
  type Outcome,
  type Transport,
  writeMember,
} from "@wamn/web-runtime";
import {
  Button,
  DataTable,
  TableScreen,
  announceOutcome,
  createTableLoad,
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

/** What the table for `wamn-receiving:location/list@1.0.0` takes. */
export interface LocationListTableProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** Input the parent fixes, which the operator does not edit. */
  readonly fixed?: Partial<LocationListRequest>;
  /** Called with the values one row hands to `wamn-receiving:receiving/record-receipt@1.0.0`. */
  readonly onFillReceivingRecordReceipt?: (initial: ReceivingRecordReceiptFormInitial) => void;
  /** Called with every outcome this screen reads. */
  readonly onOutcome?: (outcome: Outcome<LocationListResult>) => void;
}

/** What an operator calls this screen. The page decides where it goes. */
export const LocationListTableLabel = "Locations";

/**
 * The table for `wamn-receiving:location/list@1.0.0`: the DataTable over `LOCATION_LIST_TABLE`.
 *
 * It loads when it mounts. A change to a filter, a sort of rows the load did
 * not read in full, a cap change and a refresh each start a new load.
 */
export function LocationListTable(props: LocationListTableProps) {
  const load = createTableLoad<LocationListRow>(LOCATION_LIST_TABLE, async () => {
    const request = { ...props.fixed } as LocationListRequest;
    const outcome = await list(props.transport, [request]);
    props.onOutcome?.(outcome);
    if (outcome.status !== "completed") {
      announceOutcome(outcome, LocationListTableLabel);
    }
    return boundedPage(outcome);
  });
  void load.load();
  onCleanup(afterWrites(props.transport, () => void load.load()));

  const actions = (row: LocationListRow) => (
    <>
      <Show when={props.onFillReceivingRecordReceipt}>
        <Button
          type="button"
          variant="outline"
          size="sm"
          onClick={() => props.onFillReceivingRecordReceipt?.(writeMember({} as ReceivingRecordReceiptFormInitial, ["value", "line"], [writeMember({}, ["locationId"], row.id)]))}
        >
          record-receipt
        </Button>
      </Show>
    </>
  );

  return (
    <TableScreen>
      <DataTable
        name="location"
        columns={LOCATION_LIST_TABLE.columns}
        rowId={LOCATION_LIST_TABLE.rowId}
        rows={load.state().rows}
        fullyRead={load.state().fullyRead}
        busy={load.state().busy}
        refusal={load.state().refusal}
        cap={load.state().cap}
        onCapChange={(cap) => void load.load(cap)}
        onRefresh={() => void load.load()}
        startedAt={load.state().startedAt}
        endedAt={load.state().endedAt}
        sortFields={LOCATION_LIST_TABLE.sortFields}
        sortMaxFields={LOCATION_LIST_TABLE.sortMaxFields}
        onSortChange={load.sortBy}
        scopeFilters={LOCATION_LIST_TABLE.scopeFilters}
        onScopeChange={() => void load.load()}
        rowActions={actions}
      />
    </TableScreen>
  );
}

/** The table definition of `wamn-receiving:location/list@1.0.0`. */
export const LOCATION_LIST_TABLE = {
  read: "list",
  rowId: ["id"],
  pageMaximum: null,
  scopeFilters: [],
  sortFields: [],
  sortDirections: [],
  sortMaxFields: 1,
  columns: [
    { field: "id", label: "id", type: "uuid", role: "key" },
    { field: "locationCode", label: "Location code", type: "text", role: "value" },
  ],
  actions: [
    { operation: "wamn-receiving:receiving/record-receipt@1.0.0", label: "record-receipt", many: true },
  ],
  childTables: [],
} as const;
