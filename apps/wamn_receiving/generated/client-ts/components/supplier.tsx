// @generated from the client-contract IR; do not edit.
//
// `supplier` components. Each one calls the bindings and the runtime, and
// nothing else.

import { Show, createSignal, onCleanup } from "solid-js";
import { createForm } from "@tanstack/solid-form";
import { z } from "zod";
import {
  afterWrites,
  checkedMember,
  newIdempotencyKey,
  newRequestId,
  refusalMarks,
  refusalSentence,
  refusedMember,
  type Outcome,
  type Transport,
  writeMember,
} from "@wamn/web-runtime";
import {
  Button,
  DataTable,
  FieldError,
  FieldGroup,
  FormActions,
  FormDone,
  TableScreen,
  TextField,
  announceOutcome,
  createTableLoad,
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

/** What the table for `wamn-receiving:supplier/query@1.0.0` takes. */
export interface SupplierQueryTableProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** Input the parent fixes, which the operator does not edit. */
  readonly fixed?: Partial<SupplierQueryRequest>;
  /** Called with the values one row hands to `wamn-receiving:purchase-order/update@1.0.0`. */
  readonly onFillPurchaseOrderUpdate?: (initial: PurchaseOrderUpdateFormInitial) => void;
  /** Called with every outcome this screen reads. */
  readonly onOutcome?: (outcome: Outcome<SupplierQueryResult>) => void;
}

/** What an operator calls this screen. The page decides where it goes. */
export const SupplierQueryTableLabel = "Suppliers";

/**
 * The table for `wamn-receiving:supplier/query@1.0.0`: the DataTable over `SUPPLIER_QUERY_TABLE`.
 *
 * It loads when it mounts. A change to a filter, a sort of rows the load did
 * not read in full, a cap change and a refresh each start a new load.
 */
export function SupplierQueryTable(props: SupplierQueryTableProps) {
  const load = createTableLoad<SupplierQueryRow>(SUPPLIER_QUERY_TABLE, async (limit) => {
    let request = { ...props.fixed } as SupplierQueryRequest;
    request = writeMember(request, ["limit"], limit) as SupplierQueryRequest;
    const outcome = await query(props.transport, [request]);
    props.onOutcome?.(outcome);
    if (outcome.status !== "completed") {
      announceOutcome(outcome, SupplierQueryTableLabel);
    }
    return outcome;
  });
  void load.load();
  onCleanup(afterWrites(props.transport, () => void load.load()));

  const actions = (row: SupplierQueryRow) => (
    <>
      <Show when={props.onFillPurchaseOrderUpdate}>
        <Button
          type="button"
          variant="outline"
          size="sm"
          onClick={() => props.onFillPurchaseOrderUpdate?.(writeMember({} as PurchaseOrderUpdateFormInitial, ["change", "supplierId"], row.id))}
        >
          update
        </Button>
      </Show>
    </>
  );

  return (
    <TableScreen>
      <DataTable
        name="supplier"
        columns={SUPPLIER_QUERY_TABLE.columns}
        rowId={SUPPLIER_QUERY_TABLE.rowId}
        rows={load.state().rows}
        fullyRead={load.state().fullyRead}
        busy={load.state().busy}
        refusal={load.state().refusal}
        cap={load.state().cap}
        onCapChange={(cap) => void load.load(cap)}
        onRefresh={() => void load.load()}
        startedAt={load.state().startedAt}
        endedAt={load.state().endedAt}
        sortFields={SUPPLIER_QUERY_TABLE.sortFields}
        sortMaxFields={SUPPLIER_QUERY_TABLE.sortMaxFields}
        onSortChange={load.sortBy}
        scopeFilters={SUPPLIER_QUERY_TABLE.scopeFilters}
        onScopeChange={() => void load.load()}
        rowActions={actions}
      />
    </TableScreen>
  );
}

/** The table definition of `wamn-receiving:supplier/query@1.0.0`. */
export const SUPPLIER_QUERY_TABLE = {
  read: "query",
  rowId: ["id"],
  pageMaximum: 100,
  scopeFilters: [],
  sortFields: [],
  sortDirections: [],
  sortMaxFields: 1,
  columns: [
    { field: "createdAt", label: "Added", type: "timestamptz", role: "value" },
    { field: "id", label: "id", type: "uuid", role: "key" },
    { field: "name", label: "Supplier name", type: "text", role: "value" },
  ],
  actions: [
    { operation: "wamn-receiving:purchase-order/update@1.0.0", label: "update", many: false },
  ],
  childTables: [
    { definition: "PURCHASE_ORDER_QUERY_TABLE", scopeFilter: "supplierId" },
  ],
} as const;
