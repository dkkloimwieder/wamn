// @generated from the client-contract IR; do not edit.
//
// `location` components. Each one calls the bindings and the runtime, and
// nothing else.
import {
  type Outcome,
  type Transport,
} from "@wamn/web-runtime";
import {
  QueryTable,
  TableScreen,
} from "@wamn/ui";
import {
  LOCATION_LIST_REQUEST_FIELDS,
  LOCATION_LIST_RESULT_FIELDS,
  LOCATION_LIST_ROUTE,
  type LocationListRequest,
  type LocationListResult,
  type LocationListRow,
} from "../location.js";
import {
  ReceivingRecordReceiptForm,
} from "./receiving.js";

/** What the table for `wamn-receiving:location/list@1.0.0` takes. */
export interface LocationListTableProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** Input the parent fixes, which the operator does not edit. */
  readonly fixed?: Partial<LocationListRequest>;
  /** For each operation whose record a row opens, what opening it does. A row shows a button only for these. */
  readonly onOpen?: { readonly [operation: string]: (row: LocationListRow) => void };
  /** For each operation whose form a row fills, what the filled values do. */
  readonly onFill?: { readonly [operation: string]: (initial: object) => void };
  /** Called with every outcome this screen reads. */
  readonly onOutcome?: (outcome: Outcome<LocationListResult>) => void;
}

/** What an operator calls this screen. The page decides where it goes. */
export const LocationListTableLabel = "Locations";

/** The table for `wamn-receiving:location/list@1.0.0`: the QueryTable over `LOCATION_LIST_TABLE`, in the table screen. */
export function LocationListTable(props: LocationListTableProps) {
  return (
    <TableScreen>
      <QueryTable<LocationListRow, LocationListResult> definition={LOCATION_LIST_TABLE} label={LocationListTableLabel} {...props} />
    </TableScreen>
  );
}

/** The table definition of `wamn-receiving:location/list@1.0.0`. */
export const LOCATION_LIST_TABLE = {
  name: "location",
  read: { route: LOCATION_LIST_ROUTE, request: LOCATION_LIST_REQUEST_FIELDS, result: LOCATION_LIST_RESULT_FIELDS },
  rows: "rows",
  rowId: ["id"],
  pageMaximum: null,
  limitInput: null,
  sortFieldInput: null,
  sortDirectionInput: null,
  filters: [],
  scopeFilters: [],
  sortFields: [],
  sortDirections: [],
  sortMaxFields: 1,
  columns: [
    { field: "id", label: "id", type: "uuid", role: "key" },
    { field: "locationCode", label: "Location code", type: "text", role: "value" },
  ],
  actions: [
    { operation: "wamn-receiving:receiving/record-receipt@1.0.0", label: "record-receipt", many: true, opens: "form", fill: [{ field: "id", input: ["value", "line", "[]", "locationId"] }], form: () => ReceivingRecordReceiptForm },
  ],
  childTables: [],
} as const;
