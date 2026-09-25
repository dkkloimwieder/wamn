// @generated from the client-contract IR; do not edit.
//
// `supplier` components. Each one calls the bindings and the runtime, and
// nothing else.

import { Show, createSignal } from "solid-js";
import { createTable, type ColumnDef } from "@tanstack/solid-table";
import { createForm } from "@tanstack/solid-form";
import { z } from "zod";
import {
  appendPage,
  cellText,
  checkedMember,
  emptyPage,
  failedRead,
  firstPage,
  hasNextPage,
  newIdempotencyKey,
  newRequestId,
  refusalMarks,
  refusalSentence,
  refusedMember,
  startRead,
  type JsonValue,
  type Outcome,
  type PageState,
  type Transport,
  writeControl,
  writeMember,
} from "@wamn/web-runtime";
import {
  Button,
  DataGrid,
  DataGridContainer,
  FieldError,
  FieldGroup,
  FormActions,
  FormDone,
  TableScreen,
  TextField,
  WindowedTable,
  announceOutcome,
  gridFeatures,
  type GridFeatures,
} from "@wamn/ui";
import {
  SUPPLIER_CREATE_REQUEST_FIELDS,
  create,
  query,
  type SupplierCreateRequest,
  type SupplierCreateResult,
  type SupplierQueryRequest,
  type SupplierQueryResult,
  type SupplierQueryRow,
} from "../supplier.js";
import {
  type PurchaseOrderUpdateFormInitial,
} from "./purchase_order.js";

/** What an operator types for `wamn-receiving:supplier/create@1.0.0`. */
const CREATE_INPUT = z.object({
  name: z.string(),
});

/** What the form for `wamn-receiving:supplier/create@1.0.0` can start with. */
export interface SupplierCreateFormInitial {
  name?: string;
}

/** What the form for `wamn-receiving:supplier/create@1.0.0` takes. */
export interface SupplierCreateFormProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** Values the form starts with. */
  readonly initial?: SupplierCreateFormInitial;
  /** Called with the outcome of every submission. */
  readonly onSubmitted?: (outcome: Outcome<SupplierCreateResult>) => void;
}

/** What an operator calls this screen. The page decides where it goes. */
export const SupplierCreateFormLabel = "Add a supplier";

/**
 * The form for `wamn-receiving:supplier/create@1.0.0`.
 *
 * It renders what the operator fills and nothing else. The reserved inputs
 * come from the runtime at submit time, and the operator never sees them.
 */
export function SupplierCreateForm(props: SupplierCreateFormProps) {
  const [refusal, setRefusal] = createSignal<{ text: string; member: string | null } | null>(null);
  const [done, setDone] = createSignal(false);

  const form = createForm(() => ({
    defaultValues: { ...props.initial } as Partial<SupplierCreateRequest>,
    onSubmit: async ({ value }: { value: Partial<SupplierCreateRequest> }) => {
      setDone(false);
      const checked = CREATE_INPUT.safeParse(value);
      if (!checked.success) {
        const issue = checked.error.issues[0];
        setRefusal({
          text: issue?.message ?? "A value is not valid.",
          member: checkedMember(
            issue?.path as (string | number)[] | undefined,
            SUPPLIER_CREATE_REQUEST_FIELDS,
          ),
        });
        return;
      }
      let item = { ...value } as SupplierCreateRequest;
      item = writeMember(item, ["idempotencyKey"], newIdempotencyKey());
      item = writeMember(item, ["requestId"], newRequestId());
      const outcome = await create(props.transport, [item]);
      props.onSubmitted?.(outcome);
      announceOutcome(outcome, SupplierCreateFormLabel);
      setRefusal(
        outcome.status === "refused"
          ? { text: refusalSentence(outcome.code), member: refusedMember(outcome.detail) }
          : null,
      );
      setDone(outcome.status === "completed");
    },
  }));

  return (
    <form
      onSubmit={(event) => {
        event.preventDefault();
        void form.handleSubmit();
      }}
    >
      <Show when={refusal()?.member === null ? refusal() : undefined}>
        <FieldError>{refusal()?.text}</FieldError>
      </Show>
      <FieldGroup>
        <form.Field name={`name`}>
          {(field) => (
            <TextField
              label="Supplier name"
              type="text"
              value={String(field().state.value ?? "")}
              onInput={(value) => field().handleChange(value)}
              error={refusalMarks(refusal()?.member ?? null, "name") ? (refusal()?.text ?? null) : null}
            />
          )}
        </form.Field>
      </FieldGroup>
      <FormActions>
        <FormDone when={done()} />
        <Button type="submit">submit</Button>
      </FormActions>
    </form>
  );
}

/** Columns of `wamn-receiving:supplier/query@1.0.0`, in contract order. */
const QUERY_COLUMNS: ColumnDef<GridFeatures, SupplierQueryRow>[] = [
  {
    accessorKey: "createdAt",
    header: "Added",
    cell: (cell) => cellText(cell.getValue() as JsonValue, "timestamptz"),
  },
  {
    accessorKey: "id",
    header: "id",
    cell: (cell) => cellText(cell.getValue() as JsonValue, "uuid"),
  },
  {
    accessorKey: "name",
    header: "Supplier name",
    cell: (cell) => cellText(cell.getValue() as JsonValue, "text"),
  },
];

/** What the table for `wamn-receiving:supplier/query@1.0.0` takes. */
export interface SupplierQueryTableProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** Input the parent fixes, which the operator does not edit. */
  readonly fixed?: Partial<SupplierQueryRequest>;
  /** Called when the operator picks one row. */
  readonly onRowSelect?: (row: SupplierQueryRow) => void;
  /** Called with the values one row hands to `wamn-receiving:purchase-order/update@1.0.0`. */
  readonly onFillPurchaseOrderUpdate?: (initial: PurchaseOrderUpdateFormInitial) => void;
  /** Called with every outcome this screen reads. */
  readonly onOutcome?: (outcome: Outcome<SupplierQueryResult>) => void;
}

/** What an operator calls this screen. The page decides where it goes. */
export const SupplierQueryTableLabel = "Suppliers";

/**
 * The table for `wamn-receiving:supplier/query@1.0.0`.
 *
 * It owns its page controls and its rows. A change to a control clears the
 * rows, because a cursor names a position in the list the old input produced.
 *
 * It reads when the operator asks, and not when it mounts, because a read is
 * a request that the operator did not send yet.
 */
export function SupplierQueryTable(props: SupplierQueryTableProps) {
  const [controls, setControls] = createSignal<Partial<SupplierQueryRequest>>({});
  const [page, setPage] = createSignal<PageState<SupplierQueryRow>>(emptyPage<SupplierQueryRow>());

  const read = async (cursor: string | null) => {
    setPage(startRead(page()));
    const request = {
      ...controls(),
      ...props.fixed,
    } as SupplierQueryRequest;
    const sent = cursor === null ? request : (writeMember(request, ["cursor"], cursor) as SupplierQueryRequest);
    const outcome = await query(props.transport, [sent]);
    props.onOutcome?.(outcome);
    if (outcome.status !== "completed") {
      announceOutcome(outcome, SupplierQueryTableLabel);
      setPage(failedRead(page(), outcome));
      return;
    }
    const rows = outcome.value.item;
    setPage(cursor === null ? firstPage(rows, outcome.value.nextCursor) : appendPage(page(), rows, outcome.value.nextCursor));
  };

  const restart = () => {
    setPage(emptyPage<SupplierQueryRow>());
    void read(null);
  };

  const change = (path: readonly string[], value: JsonValue) => {
    setControls((current) => writeControl(current, path, value));
    restart();
  };

  const columns: ColumnDef<GridFeatures, SupplierQueryRow>[] = [
    ...QUERY_COLUMNS,
    {
      id: "fillPurchaseOrderUpdate",
      header: "",
      cell: (cell) => (
        <Show when={props.onFillPurchaseOrderUpdate}>
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => props.onFillPurchaseOrderUpdate?.(writeMember({} as PurchaseOrderUpdateFormInitial, ["change", "supplierId"], cell.row.original.id))}
          >
            update
          </Button>
        </Show>
      ),
    },
  ];

  const table = createTable({
    features: gridFeatures,
    get data() {
      return page().rows as SupplierQueryRow[];
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
