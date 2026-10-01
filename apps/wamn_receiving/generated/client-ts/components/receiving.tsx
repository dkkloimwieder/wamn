// @generated from the client-contract IR; do not edit.
//
// `receiving` components. Each one calls the bindings and the runtime, and
// nothing else.

import { Index, Show, createEffect, createSignal, onCleanup } from "solid-js";
import * as z from "zod/mini";
import {
  afterWrites,
  appendPage,
  callEach,
  canAdd,
  canRemove,
  checkedMember,
  emptyPage,
  firstPage,
  hasNextPage,
  mergeMembers,
  newIdempotencyKey,
  newRequestId,
  occurredAt,
  readMember,
  refusalMarks,
  refusalMarksGroup,
  refusalSentence,
  refusedMember,
  type JsonValue,
  type Numeric,
  type Outcome,
  type PageState,
  type Transport,
  type Uuid,
  writeMember,
} from "@wamn/web-runtime";
import {
  Button,
  FieldError,
  FieldGroup,
  FieldLegend,
  FieldSet,
  FormActions,
  FormDone,
  QueryTable,
  RecordSelect,
  TableScreen,
  TextField,
  announceOutcome,
} from "@wamn/ui";
import {
  RECEIVING_LOAD_PURCHASE_ORDER_HISTORY_REQUEST_FIELDS,
  RECEIVING_LOAD_PURCHASE_ORDER_HISTORY_RESULT_FIELDS,
  RECEIVING_LOAD_PURCHASE_ORDER_HISTORY_ROUTE,
  RECEIVING_LOAD_RECEIPT_SCREEN_REQUEST_FIELDS,
  RECEIVING_LOAD_RECEIPT_SCREEN_RESULT_FIELDS,
  RECEIVING_LOAD_RECEIPT_SCREEN_ROUTE,
  RECEIVING_RECORD_RECEIPT_REQUEST_FIELDS,
  RECEIVING_RECORD_RECEIPT_RESULT_FIELDS,
  RECEIVING_RECORD_RECEIPT_ROUTE,
  loadReceiptScreen as receivingLoadReceiptScreen,
  recordReceipt,
  type ReceivingLoadPurchaseOrderHistoryRequest,
  type ReceivingLoadPurchaseOrderHistoryResult,
  type ReceivingLoadPurchaseOrderHistoryRow,
  type ReceivingLoadReceiptScreenRequest,
  type ReceivingLoadReceiptScreenResult,
  type ReceivingLoadReceiptScreenRow,
  type ReceivingRecordReceiptRequest,
  type ReceivingRecordReceiptResult,
} from "../receiving.js";
import {
  ReceivingLoadPurchaseOrderHistoryTableLabel,
  ReceivingLoadReceiptScreenTableLabel,
  ReceivingRecordReceiptFormLabel,
} from "./labels.js";
import {
  list as locationList,
  type LocationListRequest,
  type LocationListRow,
} from "../location.js";
import {
  get as purchaseOrderGet,
  query as purchaseOrderQuery,
  type PurchaseOrderGetRequest,
  type PurchaseOrderQueryRequest,
  type PurchaseOrderQueryRow,
} from "../purchase_order.js";

/** What the release accepts: one UUID, hyphenated. */
const UUID_TEXT = /^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$/;

/** What the release accepts: decimal text without an exponent. */
const NUMERIC_TEXT = /^[+-]?(\d+(\.\d*)?|\.\d+)$/;

/** What the table for `wamn-receiving:receiving/load-purchase-order-history@2.0.0` takes. */
export interface ReceivingLoadPurchaseOrderHistoryTableProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** Input the parent fixes, which the operator does not edit. */
  readonly fixed?: Partial<ReceivingLoadPurchaseOrderHistoryRequest>;
  /** For each operation whose record a row opens, what opening it does. A row shows a button only for these. */
  readonly onOpen?: { readonly [operation: string]: (row: ReceivingLoadPurchaseOrderHistoryRow) => void };
  /** For each operation whose form a row fills, what the filled values do. */
  readonly onFill?: { readonly [operation: string]: (initial: object) => void };
  /** Called with every outcome this screen reads. */
  readonly onOutcome?: (outcome: Outcome<ReceivingLoadPurchaseOrderHistoryResult>) => void;
}

/** The table for `wamn-receiving:receiving/load-purchase-order-history@2.0.0`: the QueryTable over `RECEIVING_LOAD_PURCHASE_ORDER_HISTORY_TABLE`, in the table screen. */
export function ReceivingLoadPurchaseOrderHistoryTable(props: ReceivingLoadPurchaseOrderHistoryTableProps) {
  return (
    <TableScreen>
      <QueryTable<ReceivingLoadPurchaseOrderHistoryRow, ReceivingLoadPurchaseOrderHistoryResult> definition={RECEIVING_LOAD_PURCHASE_ORDER_HISTORY_TABLE} label={ReceivingLoadPurchaseOrderHistoryTableLabel} {...props} />
    </TableScreen>
  );
}

/** The table definition of `wamn-receiving:receiving/load-purchase-order-history@2.0.0`. */
export const RECEIVING_LOAD_PURCHASE_ORDER_HISTORY_TABLE = {
  name: "receiving",
  read: { route: RECEIVING_LOAD_PURCHASE_ORDER_HISTORY_ROUTE, request: RECEIVING_LOAD_PURCHASE_ORDER_HISTORY_REQUEST_FIELDS, result: RECEIVING_LOAD_PURCHASE_ORDER_HISTORY_RESULT_FIELDS },
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
    { field: "after", label: "After", type: "text", role: "value" },
    { field: "before", label: "Before", type: "text", role: "value" },
    { field: "changedAt", label: "Changed at", type: "timestamptz", role: "value" },
    { field: "changedBy", label: "Changed by", type: "uuid", role: "value" },
    { field: "current", label: "Current", type: "text", role: "value" },
    { field: "cursor", label: "Position", type: "text", role: "value" },
    { field: "id", label: "Entry", type: "uuid", role: "key" },
    { field: "operation", label: "Operation", type: "text", role: "value" },
    { field: "type", label: "Change", type: "text", role: "value" },
  ],
  actions: [],
  childTables: [],
} as const;

/** What the table for `wamn-receiving:receiving/load-receipt-screen@2.0.0` takes. */
export interface ReceivingLoadReceiptScreenTableProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** Input the parent fixes, which the operator does not edit. */
  readonly fixed?: Partial<ReceivingLoadReceiptScreenRequest>;
  /** For each operation whose record a row opens, what opening it does. A row shows a button only for these. */
  readonly onOpen?: { readonly [operation: string]: (row: ReceivingLoadReceiptScreenRow) => void };
  /** For each operation whose form a row fills, what the filled values do. */
  readonly onFill?: { readonly [operation: string]: (initial: object) => void };
  /** Called with every outcome this screen reads. */
  readonly onOutcome?: (outcome: Outcome<ReceivingLoadReceiptScreenResult>) => void;
}

/** The table for `wamn-receiving:receiving/load-receipt-screen@2.0.0`: the QueryTable over `RECEIVING_LOAD_RECEIPT_SCREEN_TABLE`, in the table screen. */
export function ReceivingLoadReceiptScreenTable(props: ReceivingLoadReceiptScreenTableProps) {
  return (
    <TableScreen>
      <QueryTable<ReceivingLoadReceiptScreenRow, ReceivingLoadReceiptScreenResult> definition={RECEIVING_LOAD_RECEIPT_SCREEN_TABLE} label={ReceivingLoadReceiptScreenTableLabel} {...props} />
    </TableScreen>
  );
}

/** The table definition of `wamn-receiving:receiving/load-receipt-screen@2.0.0`. */
export const RECEIVING_LOAD_RECEIPT_SCREEN_TABLE = {
  name: "receiving",
  read: { route: RECEIVING_LOAD_RECEIPT_SCREEN_ROUTE, request: RECEIVING_LOAD_RECEIPT_SCREEN_REQUEST_FIELDS, result: RECEIVING_LOAD_RECEIPT_SCREEN_RESULT_FIELDS },
  rows: "rows",
  rowId: ["lineId"],
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
    { field: "itemId", label: "item id", type: "uuid", role: "value" },
    { field: "itemNumber", label: "Item", type: "text", role: "value" },
    { field: "lineId", label: "line id", type: "uuid", role: "key" },
    { field: "lineNumber", label: "Line", type: "int32", role: "value" },
    { field: "orderedQuantity", label: "Ordered", type: "numeric", role: "value" },
    { field: "purchaseOrderId", label: "purchase order id", type: "uuid", role: "value" },
    { field: "purchaseOrderNumber", label: "Order number", type: "text", role: "value" },
    { field: "purchaseOrderStatus", label: "Status", type: "text", role: "value" },
    { field: "receivedQuantity", label: "Received", type: "numeric", role: "value" },
    { field: "remainingQuantity", label: "Remaining", type: "numeric", role: "value" },
    { field: "rowVersion", label: "Revision", type: "int32", role: "revision" },
    { field: "supplierId", label: "Supplier", type: "uuid", role: "value" },
  ],
  actions: [
    { operation: "wamn-receiving:receiving/record-receipt@2.0.0", reference: "wamn-receiving:receiving/record-receipt", label: "record-receipt", many: true, opens: "form", fill: [{ field: "lineId", input: ["value", "line", "[]", "purchaseOrderLineId"] }], form: async () => ({ default: ReceivingRecordReceiptForm }) },
  ],
  childTables: [],
} as const;

/** What an operator types for `wamn-receiving:receiving/record-receipt@2.0.0`. */
const RECORD_RECEIPT_INPUT = z.object({
  value: z.optional(
    z.object({
      line: z.optional(
        z.array(
          z.object({
            locationId: z.string().check(z.regex(UUID_TEXT, "expected a UUID")),
            purchaseOrderLineId: z.string().check(z.regex(UUID_TEXT, "expected a UUID")),
            quantity: z.string().check(z.regex(NUMERIC_TEXT, "expected decimal text")),
          }),
        ).check(z.minLength(1), z.maxLength(100)),
      ),
      purchaseOrderId: z.string().check(z.regex(UUID_TEXT, "expected a UUID")),
      receiptReference: z.string(),
    }),
  ),
});

/** What the form for `wamn-receiving:receiving/record-receipt@2.0.0` can start with. */
export interface ReceivingRecordReceiptFormInitial {
  value?: {
    line?: {
      locationId?: Uuid;
      purchaseOrderLineId?: Uuid;
      quantity?: Numeric;
    }[];
    purchaseOrderId?: Uuid;
    receiptReference?: string;
  };
}

/** What the form for `wamn-receiving:receiving/record-receipt@2.0.0` takes. */
export interface ReceivingRecordReceiptFormProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** Values the form starts with. */
  readonly initial?: ReceivingRecordReceiptFormInitial;
  /** Called with the outcome of every submission. */
  readonly onSubmitted?: (outcome: Outcome<ReceivingRecordReceiptResult>) => void;
  /**
   * The values each selected row fills. One submission then sends one input
   * for each row, in one call, and the form hides the inputs the rows fill.
   */
  readonly rows?: readonly object[];
  /** Called with the outcome of each row's input, in row order. */
  readonly onEach?: (outcomes: readonly Outcome<ReceivingRecordReceiptResult>[]) => void;
}

/**
 * The form for `wamn-receiving:receiving/record-receipt@2.0.0`.
 *
 * It renders what the operator fills and nothing else. The reserved inputs
 * come from the runtime at submit time, and the operator never sees them.
 */
export function ReceivingRecordReceiptForm(props: ReceivingRecordReceiptFormProps) {
  const [refusal, setRefusal] = createSignal<{ text: string; member: string | null } | null>(null);
  const [done, setDone] = createSignal(false);

  const [valueLineValue, setValueLineValue] = createSignal<JsonValue[] | undefined>(
    readMember(props.initial, ["value", "line"]) as JsonValue[] | undefined,
  );
  const setValueLineMember = (index: number, path: readonly string[], member: JsonValue) =>
    setValueLineValue((list) =>
      (list ?? []).map((element, at) => (at === index ? writeMember(element, path, member) : element)),
    );
  const [valuePurchaseOrderIdValue, setValuePurchaseOrderIdValue] = createSignal<JsonValue | undefined>(
    readMember(props.initial, ["value", "purchaseOrderId"]) as JsonValue | undefined,
  );
  const [valueReceiptReferenceValue, setValueReceiptReferenceValue] = createSignal<JsonValue | undefined>(
    readMember(props.initial, ["value", "receiptReference"]) as JsonValue | undefined,
  );

  const submit = async () => {
    setDone(false);
    let value = { ...props.initial } as Partial<ReceivingRecordReceiptRequest>;
    const hold = (path: readonly string[], member: JsonValue | undefined) => {
      if (member !== undefined) {
        value = writeMember(value, path, member);
      }
    };
    hold(["value", "line"], valueLineValue());
    hold(["value", "purchaseOrderId"], valuePurchaseOrderIdValue());
    hold(["value", "receiptReference"], valueReceiptReferenceValue());
    if (props.rows !== undefined) {
      const items: ReceivingRecordReceiptRequest[] = [];
      for (const row of props.rows) {
        const each = mergeMembers(value, row);
        const checked = RECORD_RECEIPT_INPUT.safeParse(each);
        if (!checked.success) {
          const issue = checked.error.issues[0];
          setRefusal({
            text: issue?.message ?? "A value is not valid.",
            member: checkedMember(
              issue?.path as (string | number)[] | undefined,
              RECEIVING_RECORD_RECEIPT_REQUEST_FIELDS,
            ),
          });
          return;
        }
        let item = { ...each } as ReceivingRecordReceiptRequest;
        item = writeMember(item, ["requestId"], newRequestId());
        item = writeMember(item, ["value", "idempotencyKey"], newIdempotencyKey());
        item = writeMember(item, ["value", "occurredAt"], occurredAt());
        items.push(item);
      }
      props.onEach?.(
        await callEach<ReceivingRecordReceiptResult>(
          props.transport,
          { route: RECEIVING_RECORD_RECEIPT_ROUTE, request: RECEIVING_RECORD_RECEIPT_REQUEST_FIELDS, result: RECEIVING_RECORD_RECEIPT_RESULT_FIELDS },
          items,
        ),
      );
      return;
    }
    const checked = RECORD_RECEIPT_INPUT.safeParse(value);
    if (!checked.success) {
      const issue = checked.error.issues[0];
      setRefusal({
        text: issue?.message ?? "A value is not valid.",
        member: checkedMember(
          issue?.path as (string | number)[] | undefined,
          RECEIVING_RECORD_RECEIPT_REQUEST_FIELDS,
        ),
      });
      return;
    }
    let item = { ...value } as ReceivingRecordReceiptRequest;
    item = writeMember(item, ["requestId"], newRequestId());
    item = writeMember(item, ["value", "idempotencyKey"], newIdempotencyKey());
    item = writeMember(item, ["value", "occurredAt"], occurredAt());
    const outcome = await recordReceipt(props.transport, [item]);
    props.onSubmitted?.(outcome);
    announceOutcome(outcome, ReceivingRecordReceiptFormLabel);
    setRefusal(
      outcome.status === "refused"
        ? { text: refusalSentence(outcome.code, outcome.text), member: refusedMember(outcome.detail) }
        : null,
    );
    setDone(outcome.status === "completed");
  };
  const [valueLineLocationIdOptions, setValueLineLocationIdOptions] = createSignal<PageState<LocationListRow>>(emptyPage<LocationListRow>());
  const readValueLineLocationIdOptions = async (cursor: string | null) => {
    const request = {} as LocationListRequest;
    const outcome = await locationList(props.transport, [request]);
    if (outcome.status !== "completed") {
      return;
    }
    const rows = outcome.value.rows as LocationListRow[];
    setValueLineLocationIdOptions(
      cursor === null
        ? firstPage(rows, null)
        : appendPage(valueLineLocationIdOptions(), rows, null),
    );
  };
  void readValueLineLocationIdOptions(null);
  onCleanup(afterWrites(props.transport, () => void readValueLineLocationIdOptions(null)));
  const [valueLinePurchaseOrderLineIdOptions, setValueLinePurchaseOrderLineIdOptions] = createSignal<PageState<ReceivingLoadReceiptScreenRow>>(emptyPage<ReceivingLoadReceiptScreenRow>());
  const [valueLinePurchaseOrderLineIdNarrowed, setValueLinePurchaseOrderLineIdNarrowed] = createSignal<string | null>(null);
  const readValueLinePurchaseOrderLineIdOptions = async (cursor: string | null) => {
    const narrowed = valueLinePurchaseOrderLineIdNarrowed();
    if (narrowed === null || narrowed === "") {
      setValueLinePurchaseOrderLineIdOptions(emptyPage<ReceivingLoadReceiptScreenRow>());
      return;
    }
    const request = { purchaseOrderId: narrowed } as ReceivingLoadReceiptScreenRequest;
    const outcome = await receivingLoadReceiptScreen(props.transport, [request]);
    if (outcome.status !== "completed") {
      return;
    }
    const rows = outcome.value.rows as ReceivingLoadReceiptScreenRow[];
    setValueLinePurchaseOrderLineIdOptions(
      cursor === null
        ? firstPage(rows, null)
        : appendPage(valueLinePurchaseOrderLineIdOptions(), rows, null),
    );
  };
  createEffect(() => {
    setValueLinePurchaseOrderLineIdNarrowed((valuePurchaseOrderIdValue() as string | null | undefined) ?? null);
    void readValueLinePurchaseOrderLineIdOptions(null);
  });
  onCleanup(afterWrites(props.transport, () => void readValueLinePurchaseOrderLineIdOptions(null)));
  const [valuePurchaseOrderIdOptions, setValuePurchaseOrderIdOptions] = createSignal<PageState<PurchaseOrderQueryRow>>(emptyPage<PurchaseOrderQueryRow>());
  const [valuePurchaseOrderIdSearch, setValuePurchaseOrderIdSearch] = createSignal("");
  const readValuePurchaseOrderIdOptions = async (cursor: string | null) => {
    let request = {} as PurchaseOrderQueryRequest;
    if (valuePurchaseOrderIdSearch() !== "") {
      request = writeMember(request, ["filter", "purchaseOrderNumber"], [valuePurchaseOrderIdSearch()]) as PurchaseOrderQueryRequest;
    }
    if (cursor !== null) {
      request = writeMember(request, ["cursor"], cursor) as PurchaseOrderQueryRequest;
    }
    const outcome = await purchaseOrderQuery(props.transport, [request]);
    if (outcome.status !== "completed") {
      return;
    }
    const rows = outcome.value.item as PurchaseOrderQueryRow[];
    setValuePurchaseOrderIdOptions(
      cursor === null
        ? firstPage(rows, outcome.value.nextCursor)
        : appendPage(valuePurchaseOrderIdOptions(), rows, outcome.value.nextCursor),
    );
  };
  const readValuePurchaseOrderIdRecord = async (key: string): Promise<PurchaseOrderQueryRow | null> => {
    const request = writeMember({}, ["id"], key) as PurchaseOrderGetRequest;
    const outcome = await purchaseOrderGet(props.transport, [request]);
    return outcome.status === "completed" ? ((outcome.value ?? null) as unknown as PurchaseOrderQueryRow | null) : null;
  };
  void readValuePurchaseOrderIdOptions(null);
  onCleanup(afterWrites(props.transport, () => void readValuePurchaseOrderIdOptions(null)));
  const rowsFill = (path: readonly string[]) =>
    props.rows?.some((row) => readMember(row, path) !== undefined) ?? false;

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
        <FieldSet>
          <FieldLegend>Receipt lines</FieldLegend>
          <Show when={refusalMarksGroup(refusal()?.member ?? null, "value.line")}>
            <FieldError>{refusal()?.text}</FieldError>
          </Show>
          <Index each={valueLineValue() ?? []}>
            {(element, index) => (
              <FieldGroup>
                <RecordSelect
                  label="Location"
                  options={valueLineLocationIdOptions().rows}
                  optionValue={(row) => String(row.id)}
                  optionLabel={(row) => String(row.locationCode)}
                  value={readMember(element(), ["locationId"]) == null ? null : String(readMember(element(), ["locationId"]))}
                  onChange={(value) => setValueLineMember(index, ["locationId"], value ?? "")}
                  error={refusalMarks(refusal()?.member ?? null, "value.line[].location_id", index) ? (refusal()?.text ?? null) : null}
                />
                <RecordSelect
                  label="Order line"
                  options={valueLinePurchaseOrderLineIdOptions().rows}
                  optionValue={(row) => String(row.lineId)}
                  optionLabel={(row) => String(row.itemNumber)}
                  value={readMember(element(), ["purchaseOrderLineId"]) == null ? null : String(readMember(element(), ["purchaseOrderLineId"]))}
                  onChange={(value) => setValueLineMember(index, ["purchaseOrderLineId"], value ?? "")}
                  error={refusalMarks(refusal()?.member ?? null, "value.line[].purchase_order_line_id", index) ? (refusal()?.text ?? null) : null}
                />
                <TextField
                  label="Quantity"
                  type="text"
                  value={String(readMember(element(), ["quantity"]) ?? "")}
                  onInput={(value) => setValueLineMember(index, ["quantity"], value)}
                  error={refusalMarks(refusal()?.member ?? null, "value.line[].quantity", index) ? (refusal()?.text ?? null) : null}
                />
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  disabled={!canRemove(valueLineValue() ?? [], 1)}
                  onClick={() => setValueLineValue((list) => (list ?? []).filter((_, at) => at !== index))}
                >
                  remove
                </Button>
              </FieldGroup>
            )}
          </Index>
          <Button
            type="button"
            variant="outline"
            disabled={!canAdd(valueLineValue() ?? [], 100)}
            onClick={() => setValueLineValue((list) => [...(list ?? []), {}])}
          >
            add
          </Button>
        </FieldSet>
        <Show when={!rowsFill(["value", "purchaseOrderId"])}>
          <RecordSelect
            label="Purchase order"
            options={valuePurchaseOrderIdOptions().rows}
            optionValue={(row) => String(row.id)}
            optionLabel={(row) => String(row.purchaseOrderNumber)}
            value={valuePurchaseOrderIdValue() == null ? null : String(valuePurchaseOrderIdValue())}
            onChange={(value) => setValuePurchaseOrderIdValue(value ?? "")}
            onSearch={(text) => {
              setValuePurchaseOrderIdSearch(text);
              void readValuePurchaseOrderIdOptions(null);
            }}
            hasNextPage={hasNextPage(valuePurchaseOrderIdOptions())}
            onNextPage={() => void readValuePurchaseOrderIdOptions(valuePurchaseOrderIdOptions().cursor)}
            readRow={readValuePurchaseOrderIdRecord}
            error={refusalMarks(refusal()?.member ?? null, "value.purchase_order_id") ? (refusal()?.text ?? null) : null}
          />
        </Show>
        <Show when={!rowsFill(["value", "receiptReference"])}>
          <TextField
            label="Receipt reference"
            type="text"
            value={String(valueReceiptReferenceValue() ?? "")}
            onInput={(value) => setValueReceiptReferenceValue(value)}
            error={refusalMarks(refusal()?.member ?? null, "value.receipt_reference") ? (refusal()?.text ?? null) : null}
          />
        </Show>
      </FieldGroup>
      <FormActions>
        <FormDone when={done()} />
        <Button type="submit">submit</Button>
      </FormActions>
    </form>
  );
}
