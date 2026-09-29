// @generated from the client-contract IR; do not edit.
//
// `supplier` components. Each one calls the bindings and the runtime, and
// nothing else.

import { Show, createSignal } from "solid-js";
import * as z from "zod/mini";
import {
  checkedMember,
  newIdempotencyKey,
  newRequestId,
  readMember,
  refusalMarks,
  refusalSentence,
  refusedMember,
  type JsonValue,
  type Outcome,
  type Transport,
  writeMember,
} from "@wamn/web-runtime";
import {
  Button,
  FieldError,
  FieldGroup,
  FormActions,
  FormDone,
  QueryTable,
  TableScreen,
  TextField,
  announceOutcome,
} from "@wamn/ui";
import {
  SUPPLIER_CREATE_REQUEST_FIELDS,
  SUPPLIER_QUERY_REQUEST_FIELDS,
  SUPPLIER_QUERY_RESULT_FIELDS,
  SUPPLIER_QUERY_ROUTE,
  create,
  type SupplierCreateRequest,
  type SupplierCreateResult,
  type SupplierQueryRequest,
  type SupplierQueryResult,
  type SupplierQueryRow,
} from "../supplier.js";
import {
  SupplierCreateFormLabel,
  SupplierQueryTableLabel,
} from "./labels.js";
import {
  PURCHASE_ORDER_QUERY_TABLE,
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

/**
 * The form for `wamn-receiving:supplier/create@1.0.0`.
 *
 * It renders what the operator fills and nothing else. The reserved inputs
 * come from the runtime at submit time, and the operator never sees them.
 */
export function SupplierCreateForm(props: SupplierCreateFormProps) {
  const [refusal, setRefusal] = createSignal<{ text: string; member: string | null } | null>(null);
  const [done, setDone] = createSignal(false);

  const [nameValue, setNameValue] = createSignal<JsonValue | undefined>(
    readMember(props.initial, ["name"]) as JsonValue | undefined,
  );

  const submit = async () => {
    setDone(false);
    let value = { ...props.initial } as Partial<SupplierCreateRequest>;
    const hold = (path: readonly string[], member: JsonValue | undefined) => {
      if (member !== undefined) {
        value = writeMember(value, path, member);
      }
    };
    hold(["name"], nameValue());
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
        ? { text: refusalSentence(outcome.code, outcome.text), member: refusedMember(outcome.detail) }
        : null,
    );
    setDone(outcome.status === "completed");
  };

  return (
    <form
      onSubmit={(event) => {
        event.preventDefault();
        void submit();
      }}
    >
      <Show when={refusal()?.member === null ? refusal() : undefined}>
        <FieldError>{refusal()?.text}</FieldError>
      </Show>
      <FieldGroup>
        <TextField
          label="Supplier name"
          type="text"
          value={String(nameValue() ?? "")}
          onInput={(value) => setNameValue(value)}
          error={refusalMarks(refusal()?.member ?? null, "name") ? (refusal()?.text ?? null) : null}
        />
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
  /** For each operation whose record a row opens, what opening it does. A row shows a button only for these. */
  readonly onOpen?: { readonly [operation: string]: (row: SupplierQueryRow) => void };
  /** For each operation whose form a row fills, what the filled values do. */
  readonly onFill?: { readonly [operation: string]: (initial: object) => void };
  /** Called with every outcome this screen reads. */
  readonly onOutcome?: (outcome: Outcome<SupplierQueryResult>) => void;
}

/** The table for `wamn-receiving:supplier/query@1.0.0`: the QueryTable over `SUPPLIER_QUERY_TABLE`, in the table screen. */
export function SupplierQueryTable(props: SupplierQueryTableProps) {
  return (
    <TableScreen>
      <QueryTable<SupplierQueryRow, SupplierQueryResult> definition={SUPPLIER_QUERY_TABLE} label={SupplierQueryTableLabel} {...props} />
    </TableScreen>
  );
}

/** The table definition of `wamn-receiving:supplier/query@1.0.0`. */
export const SUPPLIER_QUERY_TABLE = {
  name: "supplier",
  read: { route: SUPPLIER_QUERY_ROUTE, request: SUPPLIER_QUERY_REQUEST_FIELDS, result: SUPPLIER_QUERY_RESULT_FIELDS },
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
    { field: "createdAt", label: "Added", type: "timestamptz", role: "value" },
    { field: "id", label: "id", type: "uuid", role: "key" },
    { field: "name", label: "Supplier name", type: "text", role: "value" },
  ],
  actions: [
    { operation: "wamn-receiving:purchase-order/update@1.0.0", label: "update", many: false, opens: "form", fill: [{ field: "id", input: ["change", "supplierId"] }] },
  ],
  childTables: [
    { label: "purchase order", table: () => PURCHASE_ORDER_QUERY_TABLE, scopeFilter: "supplierId" },
  ],
} as const;
