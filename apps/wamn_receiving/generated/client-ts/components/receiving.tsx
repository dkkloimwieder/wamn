// @generated from the client-contract IR; do not edit.
//
// `receiving` components. Each one calls the bindings and the runtime, and
// nothing else.

import { For, Show, createEffect, createSignal, onCleanup } from "solid-js";
import { createForm, useStore } from "@tanstack/solid-form";
import { z } from "zod";
import {
  afterWrites,
  appendPage,
  boundedPage,
  canAdd,
  canRemove,
  checkedMember,
  emptyPage,
  firstPage,
  hasNextPage,
  newIdempotencyKey,
  newRequestId,
  occurredAt,
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
  DataTable,
  FieldError,
  FieldGroup,
  FieldLegend,
  FieldSet,
  FormActions,
  FormDone,
  RecordSelect,
  TableScreen,
  TextField,
  announceOutcome,
  createTableLoad,
} from "@wamn/ui";
import {
  RECEIVING_RECORD_RECEIPT_REQUEST_FIELDS,
  loadPurchaseOrderHistory,
  loadReceiptScreen,
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

/** What the table for `wamn-receiving:receiving/load-purchase-order-history@1.0.0` takes. */
export interface ReceivingLoadPurchaseOrderHistoryTableProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** Input the parent fixes, which the operator does not edit. */
  readonly fixed?: Partial<ReceivingLoadPurchaseOrderHistoryRequest>;
  /** Called with every outcome this screen reads. */
  readonly onOutcome?: (outcome: Outcome<ReceivingLoadPurchaseOrderHistoryResult>) => void;
}

/** What an operator calls this screen. The page decides where it goes. */
export const ReceivingLoadPurchaseOrderHistoryTableLabel = "Purchase order history";

/**
 * The table for `wamn-receiving:receiving/load-purchase-order-history@1.0.0`: the DataTable over `RECEIVING_LOAD_PURCHASE_ORDER_HISTORY_TABLE`.
 *
 * It loads when it mounts. A change to a filter, a sort of rows the load did
 * not read in full, a cap change and a refresh each start a new load.
 */
export function ReceivingLoadPurchaseOrderHistoryTable(props: ReceivingLoadPurchaseOrderHistoryTableProps) {
  const load = createTableLoad<ReceivingLoadPurchaseOrderHistoryRow>(RECEIVING_LOAD_PURCHASE_ORDER_HISTORY_TABLE, async () => {
    const request = { ...props.fixed } as ReceivingLoadPurchaseOrderHistoryRequest;
    const outcome = await loadPurchaseOrderHistory(props.transport, [request]);
    props.onOutcome?.(outcome);
    if (outcome.status !== "completed") {
      announceOutcome(outcome, ReceivingLoadPurchaseOrderHistoryTableLabel);
    }
    return boundedPage(outcome);
  });
  void load.load();
  onCleanup(afterWrites(props.transport, () => void load.load()));

  return (
    <TableScreen>
      <DataTable
        name="receiving"
        columns={RECEIVING_LOAD_PURCHASE_ORDER_HISTORY_TABLE.columns}
        rowId={RECEIVING_LOAD_PURCHASE_ORDER_HISTORY_TABLE.rowId}
        rows={load.state().rows}
        fullyRead={load.state().fullyRead}
        busy={load.state().busy}
        refusal={load.state().refusal}
        cap={load.state().cap}
        onCapChange={(cap) => void load.load(cap)}
        onRefresh={() => void load.load()}
        startedAt={load.state().startedAt}
        endedAt={load.state().endedAt}
        sortFields={RECEIVING_LOAD_PURCHASE_ORDER_HISTORY_TABLE.sortFields}
        sortMaxFields={RECEIVING_LOAD_PURCHASE_ORDER_HISTORY_TABLE.sortMaxFields}
        onSortChange={load.sortBy}
        scopeFilters={RECEIVING_LOAD_PURCHASE_ORDER_HISTORY_TABLE.scopeFilters}
        onScopeChange={() => void load.load()}
      />
    </TableScreen>
  );
}

/** The table definition of `wamn-receiving:receiving/load-purchase-order-history@1.0.0`. */
export const RECEIVING_LOAD_PURCHASE_ORDER_HISTORY_TABLE = {
  read: "loadPurchaseOrderHistory",
  rowId: null,
  pageMaximum: null,
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
    { field: "kind", label: "Change", type: "text", role: "value" },
    { field: "operation", label: "Operation", type: "text", role: "value" },
  ],
} as const;

/** What the table for `wamn-receiving:receiving/load-receipt-screen@1.0.0` takes. */
export interface ReceivingLoadReceiptScreenTableProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** Input the parent fixes, which the operator does not edit. */
  readonly fixed?: Partial<ReceivingLoadReceiptScreenRequest>;
  /** Called with the values one row hands to `wamn-receiving:receiving/record-receipt@1.0.0`. */
  readonly onFillReceivingRecordReceipt?: (initial: ReceivingRecordReceiptFormInitial) => void;
  /** Called with every outcome this screen reads. */
  readonly onOutcome?: (outcome: Outcome<ReceivingLoadReceiptScreenResult>) => void;
}

/** What an operator calls this screen. The page decides where it goes. */
export const ReceivingLoadReceiptScreenTableLabel = "Receiving screen";

/**
 * The table for `wamn-receiving:receiving/load-receipt-screen@1.0.0`: the DataTable over `RECEIVING_LOAD_RECEIPT_SCREEN_TABLE`.
 *
 * It loads when it mounts. A change to a filter, a sort of rows the load did
 * not read in full, a cap change and a refresh each start a new load.
 */
export function ReceivingLoadReceiptScreenTable(props: ReceivingLoadReceiptScreenTableProps) {
  const load = createTableLoad<ReceivingLoadReceiptScreenRow>(RECEIVING_LOAD_RECEIPT_SCREEN_TABLE, async () => {
    const request = { ...props.fixed } as ReceivingLoadReceiptScreenRequest;
    const outcome = await loadReceiptScreen(props.transport, [request]);
    props.onOutcome?.(outcome);
    if (outcome.status !== "completed") {
      announceOutcome(outcome, ReceivingLoadReceiptScreenTableLabel);
    }
    return boundedPage(outcome);
  });
  void load.load();
  onCleanup(afterWrites(props.transport, () => void load.load()));

  const actions = (row: ReceivingLoadReceiptScreenRow) => (
    <>
      <Show when={props.onFillReceivingRecordReceipt}>
        <Button
          type="button"
          variant="outline"
          size="sm"
          onClick={() => props.onFillReceivingRecordReceipt?.(writeMember({} as ReceivingRecordReceiptFormInitial, ["value", "line", "purchaseOrderLineId"], row.lineId))}
        >
          record-receipt
        </Button>
      </Show>
    </>
  );

  return (
    <TableScreen>
      <DataTable
        name="receiving"
        columns={RECEIVING_LOAD_RECEIPT_SCREEN_TABLE.columns}
        rowId={RECEIVING_LOAD_RECEIPT_SCREEN_TABLE.rowId}
        rows={load.state().rows}
        fullyRead={load.state().fullyRead}
        busy={load.state().busy}
        refusal={load.state().refusal}
        cap={load.state().cap}
        onCapChange={(cap) => void load.load(cap)}
        onRefresh={() => void load.load()}
        startedAt={load.state().startedAt}
        endedAt={load.state().endedAt}
        sortFields={RECEIVING_LOAD_RECEIPT_SCREEN_TABLE.sortFields}
        sortMaxFields={RECEIVING_LOAD_RECEIPT_SCREEN_TABLE.sortMaxFields}
        onSortChange={load.sortBy}
        scopeFilters={RECEIVING_LOAD_RECEIPT_SCREEN_TABLE.scopeFilters}
        onScopeChange={() => void load.load()}
        rowActions={actions}
      />
    </TableScreen>
  );
}

/** The table definition of `wamn-receiving:receiving/load-receipt-screen@1.0.0`. */
export const RECEIVING_LOAD_RECEIPT_SCREEN_TABLE = {
  read: "loadReceiptScreen",
  rowId: "lineId",
  pageMaximum: null,
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
} as const;

/** What an operator types for `wamn-receiving:receiving/record-receipt@1.0.0`. */
const RECORD_RECEIPT_INPUT = z.object({
  value: z
    .object({
      line: z
        .object({
          locationId: z.string().regex(UUID_TEXT, "expected a UUID"),
          purchaseOrderLineId: z.string().regex(UUID_TEXT, "expected a UUID"),
          quantity: z.string().regex(NUMERIC_TEXT, "expected decimal text"),
        })
        .array()
        .min(1)
        .max(100)
        .optional(),
      purchaseOrderId: z.string().regex(UUID_TEXT, "expected a UUID"),
      receiptReference: z.string(),
    })
    .optional(),
});

/** What the form for `wamn-receiving:receiving/record-receipt@1.0.0` can start with. */
export interface ReceivingRecordReceiptFormInitial {
  value?: {
    purchaseOrderId?: Uuid;
    receiptReference?: string;
  };
}

/** What the form for `wamn-receiving:receiving/record-receipt@1.0.0` takes. */
export interface ReceivingRecordReceiptFormProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** Values the form starts with. */
  readonly initial?: ReceivingRecordReceiptFormInitial;
  /** Called with the outcome of every submission. */
  readonly onSubmitted?: (outcome: Outcome<ReceivingRecordReceiptResult>) => void;
}

/** What an operator calls this screen. The page decides where it goes. */
export const ReceivingRecordReceiptFormLabel = "Record a receipt";

/**
 * The form for `wamn-receiving:receiving/record-receipt@1.0.0`.
 *
 * It renders what the operator fills and nothing else. The reserved inputs
 * come from the runtime at submit time, and the operator never sees them.
 */
export function ReceivingRecordReceiptForm(props: ReceivingRecordReceiptFormProps) {
  const [refusal, setRefusal] = createSignal<{ text: string; member: string | null } | null>(null);
  const [done, setDone] = createSignal(false);

  const form = createForm(() => ({
    defaultValues: { ...props.initial } as Partial<ReceivingRecordReceiptRequest>,
    onSubmit: async ({ value }: { value: Partial<ReceivingRecordReceiptRequest> }) => {
      setDone(false);
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
          ? { text: refusalSentence(outcome.code), member: refusedMember(outcome.detail) }
          : null,
      );
      setDone(outcome.status === "completed");
    },
  }));
  const formValues = useStore(form.store, (state) => state.values);
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
    formValues();
    setValueLinePurchaseOrderLineIdNarrowed((form.getFieldValue(`value.purchaseOrderId`) as string | null) ?? null);
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
        <form.Field name={"value.line"} mode="array">
          {(group) => (
            <FieldSet>
              <FieldLegend>Receipt lines</FieldLegend>
              <For each={group().state.value ?? []}>
                {(_, index) => (
                  <FieldGroup>
                    <form.Field name={`value.line[${index()}].locationId`}>
                      {(field) => (
                        <RecordSelect
                          label="Location"
                          options={valueLineLocationIdOptions().rows}
                          optionValue={(row) => String(row.id)}
                          optionLabel={(row) => String(row.locationCode)}
                          value={field().state.value == null ? null : String(field().state.value)}
                          onChange={(value) => field().handleChange(value ?? "")}
                          error={refusalMarks(refusal()?.member ?? null, "value.line[].location_id", index()) ? (refusal()?.text ?? null) : null}
                        />
                      )}
                    </form.Field>
                    <form.Field name={`value.line[${index()}].purchaseOrderLineId`}>
                      {(field) => (
                        <RecordSelect
                          label="Order line"
                          options={valueLinePurchaseOrderLineIdOptions().rows}
                          optionValue={(row) => String(row.lineId)}
                          optionLabel={(row) => String(row.itemNumber)}
                          value={field().state.value == null ? null : String(field().state.value)}
                          onChange={(value) => field().handleChange(value ?? "")}
                          error={refusalMarks(refusal()?.member ?? null, "value.line[].purchase_order_line_id", index()) ? (refusal()?.text ?? null) : null}
                        />
                      )}
                    </form.Field>
                    <form.Field name={`value.line[${index()}].quantity`}>
                      {(field) => (
                        <TextField
                          label="Quantity"
                          type="text"
                          value={String(field().state.value ?? "")}
                          onInput={(value) => field().handleChange(value)}
                          error={refusalMarks(refusal()?.member ?? null, "value.line[].quantity", index()) ? (refusal()?.text ?? null) : null}
                        />
                      )}
                    </form.Field>
                    <Button
                      type="button"
                      variant="outline"
                      size="sm"
                      disabled={!canRemove(group().state.value ?? [], 1)}
                      onClick={() => group().removeValue(index())}
                    >
                      remove
                    </Button>
                  </FieldGroup>
                )}
              </For>
              <Button
                type="button"
                variant="outline"
                disabled={!canAdd(group().state.value ?? [], 100)}
                onClick={() => group().pushValue({} as NonNullable<NonNullable<NonNullable<ReceivingRecordReceiptRequest>["value"]>["line"]>[number])}
              >
                add
              </Button>
            </FieldSet>
          )}
        </form.Field>
        <form.Field name={`value.purchaseOrderId`}>
          {(field) => (
            <RecordSelect
              label="Purchase order"
              options={valuePurchaseOrderIdOptions().rows}
              optionValue={(row) => String(row.id)}
              optionLabel={(row) => String(row.purchaseOrderNumber)}
              value={field().state.value == null ? null : String(field().state.value)}
              onChange={(value) => field().handleChange(value ?? "")}
              onSearch={(text) => {
                setValuePurchaseOrderIdSearch(text);
                void readValuePurchaseOrderIdOptions(null);
              }}
              hasNextPage={hasNextPage(valuePurchaseOrderIdOptions())}
              onNextPage={() => void readValuePurchaseOrderIdOptions(valuePurchaseOrderIdOptions().cursor)}
              readRow={readValuePurchaseOrderIdRecord}
              error={refusalMarks(refusal()?.member ?? null, "value.purchase_order_id") ? (refusal()?.text ?? null) : null}
            />
          )}
        </form.Field>
        <form.Field name={`value.receiptReference`}>
          {(field) => (
            <TextField
              label="Receipt reference"
              type="text"
              value={String(field().state.value ?? "")}
              onInput={(value) => field().handleChange(value)}
              error={refusalMarks(refusal()?.member ?? null, "value.receipt_reference") ? (refusal()?.text ?? null) : null}
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
