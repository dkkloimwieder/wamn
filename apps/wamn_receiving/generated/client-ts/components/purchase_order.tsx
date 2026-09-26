// @generated from the client-contract IR; do not edit.
//
// `purchase_order` components. Each one calls the bindings and the runtime, and
// nothing else.

import { Show, createResource, createSignal, onCleanup } from "solid-js";
import { createForm } from "@tanstack/solid-form";
import { z } from "zod";
import {
  afterWrites,
  appendPage,
  cellText,
  checkedMember,
  emptyPage,
  firstPage,
  hasNextPage,
  newRequestId,
  readMember,
  refusalMarks,
  refusalSentence,
  refusedMember,
  type Outcome,
  type PageState,
  type Transport,
  type Uuid,
  writeMember,
} from "@wamn/web-runtime";
import {
  Button,
  DetailItem,
  DetailList,
  FieldError,
  FieldGroup,
  FormActions,
  FormDone,
  QueryTable,
  RecordSelect,
  announceOutcome,
} from "@wamn/ui";
import {
  PURCHASE_ORDER_QUERY_REQUEST_FIELDS,
  PURCHASE_ORDER_QUERY_RESULT_FIELDS,
  PURCHASE_ORDER_QUERY_ROUTE,
  PURCHASE_ORDER_UPDATE_REQUEST_FIELDS,
  PURCHASE_ORDER_UPDATE_RESULT_FIELDS,
  PURCHASE_ORDER_UPDATE_ROUTE,
  get,
  type PurchaseOrderGetRequest,
  type PurchaseOrderGetResult,
  type PurchaseOrderQueryRequest,
  type PurchaseOrderQueryResult,
  type PurchaseOrderQueryRow,
  type PurchaseOrderUpdateRequest,
  type PurchaseOrderUpdateResult,
  update,
} from "../purchase_order.js";
import {
  ReceivingRecordReceiptForm,
} from "./receiving.js";
import {
  query as supplierQuery,
  type SupplierQueryRequest,
  type SupplierQueryRow,
} from "../supplier.js";

/** What the release accepts: one UUID, hyphenated. */
const UUID_TEXT = /^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$/;

/** The record that the detail for `wamn-receiving:purchase-order/get@1.0.0` reads. */
export interface PurchaseOrderGetDetailInput {
  readonly id: Uuid;
}

/** What the detail screen for `wamn-receiving:purchase-order/get@1.0.0` takes. */
export interface PurchaseOrderGetDetailProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** The input that names the record. */
  readonly input: PurchaseOrderGetDetailInput;
  /** Called with every outcome this screen reads. */
  readonly onOutcome?: (outcome: Outcome<PurchaseOrderGetResult>) => void;
}

/** What an operator calls this screen. The page decides where it goes. */
export const PurchaseOrderGetDetailLabel = "Purchase order";

/**
 * The detail screen for `wamn-receiving:purchase-order/get@1.0.0`.
 *
 * It reads when it mounts and again whenever its input changes, because the
 * input names the record it shows.
 */
export function PurchaseOrderGetDetail(props: PurchaseOrderGetDetailProps) {
  const [outcome, { refetch: readAgain }] = createResource(
    () => props.input,
    async (input: PurchaseOrderGetDetailInput) => {
      const read = await get(props.transport, [
        input as PurchaseOrderGetRequest,
      ]);
      props.onOutcome?.(read);
      if (read.status !== "completed") {
        announceOutcome(read, PurchaseOrderGetDetailLabel);
      }
      return read;
    },
  );
  onCleanup(afterWrites(props.transport, () => void readAgain()));
  const record = (): PurchaseOrderGetResult | undefined => {
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
        <DetailItem term="Created">{cellText(readMember(record(), ["createdAt"]), "timestamptz")}</DetailItem>
        <DetailItem term="Created by">{cellText(readMember(record(), ["createdBy"]), "uuid")}</DetailItem>
        <DetailItem term="id">{cellText(readMember(record(), ["id"]), "uuid")}</DetailItem>
        <DetailItem term="Order number">{cellText(readMember(record(), ["purchaseOrderNumber"]), "text")}</DetailItem>
        <DetailItem term="Revision">{cellText(readMember(record(), ["rowVersion"]), "int32")}</DetailItem>
        <DetailItem term="Status">{cellText(readMember(record(), ["status"]), "text")}</DetailItem>
        <DetailItem term="Supplier">{cellText(readMember(record(), ["supplierId"]), "uuid")}</DetailItem>
        <DetailItem term="Updated">{cellText(readMember(record(), ["updatedAt"]), "timestamptz")}</DetailItem>
        <DetailItem term="Updated by">{cellText(readMember(record(), ["updatedBy"]), "uuid")}</DetailItem>
      </DetailList>
    </section>
  );
}

/** What the table for `wamn-receiving:purchase-order/query@1.0.0` takes. */
export interface PurchaseOrderQueryTableProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** Input the parent fixes, which the operator does not edit. */
  readonly fixed?: Partial<PurchaseOrderQueryRequest>;
  /** For each operation whose record a row opens, what opening it does. A row shows a button only for these. */
  readonly onOpen?: { readonly [operation: string]: (row: PurchaseOrderQueryRow) => void };
  /** For each operation whose form a row fills, what the filled values do. */
  readonly onFill?: { readonly [operation: string]: (initial: object) => void };
  /** Called with every outcome this screen reads. */
  readonly onOutcome?: (outcome: Outcome<PurchaseOrderQueryResult>) => void;
}

/** What an operator calls this screen. The page decides where it goes. */
export const PurchaseOrderQueryTableLabel = "Purchase orders";

/** The table for `wamn-receiving:purchase-order/query@1.0.0`: the QueryTable over `PURCHASE_ORDER_QUERY_TABLE`. */
export function PurchaseOrderQueryTable(props: PurchaseOrderQueryTableProps) {
  return <QueryTable<PurchaseOrderQueryRow, PurchaseOrderQueryResult> definition={PURCHASE_ORDER_QUERY_TABLE} label={PurchaseOrderQueryTableLabel} {...props} />;
}

/** The table definition of `wamn-receiving:purchase-order/query@1.0.0`. */
export const PURCHASE_ORDER_QUERY_TABLE = {
  name: "purchase-order",
  read: { route: PURCHASE_ORDER_QUERY_ROUTE, request: PURCHASE_ORDER_QUERY_REQUEST_FIELDS, result: PURCHASE_ORDER_QUERY_RESULT_FIELDS },
  rows: "item",
  rowId: ["id"],
  pageMaximum: 100,
  limitInput: ["limit"],
  sortFieldInput: ["sort", "field"],
  sortDirectionInput: ["sort", "direction"],
  filters: [{ field: "purchaseOrderNumber", input: ["filter", "purchaseOrderNumber"], list: true }, { field: "status", input: ["filter", "status"], list: true }, { field: "supplierId", input: ["filter", "supplierId"], list: true }],
  scopeFilters: ["purchaseOrderNumber", "status", "supplierId"],
  sortFields: [{ field: "createdAt", wire: "created_at" }, { field: "purchaseOrderNumber", wire: "purchase_order_number" }, { field: "status", wire: "status" }],
  sortDirections: ["ascending", "descending"],
  sortMaxFields: 1,
  columns: [
    { field: "createdAt", label: "Created", type: "timestamptz", role: "value" },
    { field: "createdBy", label: "Created by", type: "uuid", role: "value" },
    { field: "id", label: "id", type: "uuid", role: "key" },
    { field: "purchaseOrderNumber", label: "Order number", type: "text", role: "value" },
    { field: "rowVersion", label: "Revision", type: "int32", role: "revision" },
    { field: "status", label: "Status", type: "text", role: "value" },
    { field: "supplierId", label: "Supplier", type: "uuid", role: "reference" },
    { field: "updatedAt", label: "Updated", type: "timestamptz", role: "value" },
    { field: "updatedBy", label: "Updated by", type: "uuid", role: "value" },
  ],
  update: { operation: "wamn-receiving:purchase-order/update@1.0.0", binding: { route: PURCHASE_ORDER_UPDATE_ROUTE, request: PURCHASE_ORDER_UPDATE_REQUEST_FIELDS, result: PURCHASE_ORDER_UPDATE_RESULT_FIELDS }, keyInput: ["id"], revisionInput: ["expectedRowVersion"], revisionField: "rowVersion", supplied: [{ input: ["requestId"], kind: "requestId" }], fields: [
    { field: "supplierId", input: ["change", "supplierId"] },
  ] },
  actions: [
    { operation: "wamn-receiving:purchase-order/get@1.0.0", label: "get", many: false, opens: "record", fill: [] },
    { operation: "wamn-receiving:receiving/record-receipt@1.0.0", label: "record-receipt", many: true, opens: "form", fill: [{ field: "id", input: ["value", "purchaseOrderId"] }], form: () => ReceivingRecordReceiptForm },
  ],
  childTables: [],
} as const;

/** What an operator types for `wamn-receiving:purchase-order/update@1.0.0`. */
const UPDATE_INPUT = z.object({
  change: z
    .object({
      supplierId: z.string().regex(UUID_TEXT, "expected a UUID").optional(),
    })
    .optional(),
});

/** What the form for `wamn-receiving:purchase-order/update@1.0.0` can start with. */
export interface PurchaseOrderUpdateFormInitial {
  change?: {
    supplierId?: Uuid;
  };
}

/** What the form for `wamn-receiving:purchase-order/update@1.0.0` takes. */
export interface PurchaseOrderUpdateFormProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** Values the form starts with. */
  readonly initial?: PurchaseOrderUpdateFormInitial;
  /** The record this command changes. The form reads it when it opens, and
   * sends the revision it read, because `wamn-receiving:purchase-order/get@1.0.0` states that binding. */
  readonly key: PurchaseOrderGetDetailInput;
  /** Called with the outcome of every submission. */
  readonly onSubmitted?: (outcome: Outcome<PurchaseOrderUpdateResult>) => void;
}

/** What an operator calls this screen. The page decides where it goes. */
export const PurchaseOrderUpdateFormLabel = "Change the supplier";

/**
 * The form for `wamn-receiving:purchase-order/update@1.0.0`.
 *
 * It renders what the operator fills and nothing else. The reserved inputs
 * come from the runtime at submit time, and the operator never sees them.
 */
export function PurchaseOrderUpdateForm(props: PurchaseOrderUpdateFormProps) {
  const [refusal, setRefusal] = createSignal<{ text: string; member: string | null } | null>(null);
  const [done, setDone] = createSignal(false);
  // The record this command changes, read when the form opens and again
  // when its key changes. The form sends the revision of this read, so a
  // change another writer makes after it refuses as a conflict.
  const [record, { refetch: readAgain }] = createResource(
    () => props.key,
    (key: PurchaseOrderGetDetailInput) => get(props.transport, [key]),
  );

  const form = createForm(() => ({
    defaultValues: { ...props.initial } as Partial<PurchaseOrderUpdateRequest>,
    onSubmit: async ({ value }: { value: Partial<PurchaseOrderUpdateRequest> }) => {
      setDone(false);
      const checked = UPDATE_INPUT.safeParse(value);
      if (!checked.success) {
        const issue = checked.error.issues[0];
        setRefusal({
          text: issue?.message ?? "A value is not valid.",
          member: checkedMember(
            issue?.path as (string | number)[] | undefined,
            PURCHASE_ORDER_UPDATE_REQUEST_FIELDS,
          ),
        });
        return;
      }
      let item = { ...value } as PurchaseOrderUpdateRequest;
      item = writeMember(item, ["requestId"], newRequestId());
      // The revision is the one the form read when it opened, never one read
      // now, because a change made since then is what the conflict outcome names.
      const read = record();
      if (read?.status !== "completed") {
        setRefusal({ text: "The record this form changes is not read yet.", member: null });
        return;
      }
      item = writeMember(item, ["id"], readMember(read.value, ["id"]) ?? null);
      item = writeMember(item, ["expectedRowVersion"], readMember(read.value, ["rowVersion"]) ?? null);
      const outcome = await update(props.transport, [item]);
      props.onSubmitted?.(outcome);
      if (outcome.status === "completed") {
        void readAgain();
      }
      announceOutcome(outcome, PurchaseOrderUpdateFormLabel);
      setRefusal(
        outcome.status === "refused"
          ? { text: refusalSentence(outcome.code, outcome.text), member: refusedMember(outcome.detail) }
          : null,
      );
      setDone(outcome.status === "completed");
    },
  }));
  const [changeSupplierIdOptions, setChangeSupplierIdOptions] = createSignal<PageState<SupplierQueryRow>>(emptyPage<SupplierQueryRow>());
  const readChangeSupplierIdOptions = async (cursor: string | null) => {
    let request = {} as SupplierQueryRequest;
    if (cursor !== null) {
      request = writeMember(request, ["cursor"], cursor) as SupplierQueryRequest;
    }
    const outcome = await supplierQuery(props.transport, [request]);
    if (outcome.status !== "completed") {
      return;
    }
    const rows = outcome.value.item as SupplierQueryRow[];
    setChangeSupplierIdOptions(
      cursor === null
        ? firstPage(rows, outcome.value.nextCursor)
        : appendPage(changeSupplierIdOptions(), rows, outcome.value.nextCursor),
    );
  };
  void readChangeSupplierIdOptions(null);
  onCleanup(afterWrites(props.transport, () => void readChangeSupplierIdOptions(null)));

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
        <form.Field name={`change.supplierId`}>
          {(field) => (
            <RecordSelect
              label="Supplier"
              options={changeSupplierIdOptions().rows}
              optionValue={(row) => String(row.id)}
              optionLabel={(row) => String(row.name)}
              value={field().state.value == null ? null : String(field().state.value)}
              onChange={(value) => field().handleChange(value ?? "")}
              hasNextPage={hasNextPage(changeSupplierIdOptions())}
              onNextPage={() => void readChangeSupplierIdOptions(changeSupplierIdOptions().cursor)}
              error={refusalMarks(refusal()?.member ?? null, "change.supplier_id") ? (refusal()?.text ?? null) : null}
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
