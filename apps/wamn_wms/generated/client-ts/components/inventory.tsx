// @generated from the client-contract IR; do not edit.
//
// `inventory` components. Each one calls the bindings and the runtime, and
// nothing else.

import { Show, createSignal, onCleanup } from "solid-js";
import * as z from "zod/mini";
import {
  afterWrites,
  appendPage,
  callEach,
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
  ChoiceField,
  FieldError,
  FieldGroup,
  FormActions,
  FormDone,
  QueryTable,
  RecordSelect,
  TableScreen,
  TextField,
  announceOutcome,
} from "@wamn/ui";
import {
  INVENTORY_ADJUST_REQUEST_FIELDS,
  INVENTORY_ADJUST_RESULT_FIELDS,
  INVENTORY_ADJUST_ROUTE,
  INVENTORY_AGGREGATE_REQUEST_FIELDS,
  INVENTORY_AGGREGATE_RESULT_FIELDS,
  INVENTORY_AGGREGATE_ROUTE,
  INVENTORY_MERGE_REQUEST_FIELDS,
  INVENTORY_MERGE_RESULT_FIELDS,
  INVENTORY_MERGE_ROUTE,
  INVENTORY_MOVE_REQUEST_FIELDS,
  INVENTORY_MOVE_RESULT_FIELDS,
  INVENTORY_MOVE_ROUTE,
  INVENTORY_SPLIT_REQUEST_FIELDS,
  INVENTORY_SPLIT_RESULT_FIELDS,
  INVENTORY_SPLIT_ROUTE,
  adjust,
  merge,
  move,
  split,
  type InventoryAdjustRequest,
  type InventoryAdjustResult,
  type InventoryAggregateRequest,
  type InventoryAggregateResult,
  type InventoryAggregateRow,
  type InventoryMergeRequest,
  type InventoryMergeResult,
  type InventoryMoveRequest,
  type InventoryMoveResult,
  type InventorySplitRequest,
  type InventorySplitResult,
} from "../inventory.js";
import {
  InventoryAdjustFormLabel,
  InventoryAggregateTableLabel,
  InventoryMergeFormLabel,
  InventoryMoveFormLabel,
  InventorySplitFormLabel,
} from "./labels.js";
import {
  get as locationGet,
  query as locationQuery,
  type LocationGetRequest,
  type LocationQueryRequest,
  type LocationQueryRow,
} from "../location.js";
import {
  get as packagingGet,
  query as packagingQuery,
  type PackagingGetRequest,
  type PackagingQueryRequest,
  type PackagingQueryRow,
} from "../packaging.js";
import {
  get as productGet,
  query as productQuery,
  type ProductGetRequest,
  type ProductQueryRequest,
  type ProductQueryRow,
} from "../product.js";

/** What the release accepts: one UUID, hyphenated. */
const UUID_TEXT = /^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$/;

/** What the release accepts: decimal text without an exponent. */
const NUMERIC_TEXT = /^[+-]?(\d+(\.\d*)?|\.\d+)$/;

/** What an operator types for `wamn-wms:inventory/adjust@2.0.0`. */
const ADJUST_INPUT = z.object({
  value: z.optional(
    z.object({
      packagingId: z.string().check(z.regex(UUID_TEXT, "expected a UUID")),
      productId: z.string().check(z.regex(UUID_TEXT, "expected a UUID")),
      quantity: z.string().check(z.regex(NUMERIC_TEXT, "expected decimal text")),
      reasonCode: z.string(),
      status: z.enum(["available", "held"]),
    }),
  ),
});

/** What the form for `wamn-wms:inventory/adjust@2.0.0` can start with. */
export interface InventoryAdjustFormInitial {
  value?: {
    packagingId?: Uuid;
    productId?: Uuid;
    quantity?: Numeric;
    reasonCode?: string;
    status?: "available" | "held";
  };
}

/** What the form for `wamn-wms:inventory/adjust@2.0.0` takes. */
export interface InventoryAdjustFormProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** Values the form starts with. */
  readonly initial?: InventoryAdjustFormInitial;
  /** Called with the outcome of every submission. */
  readonly onSubmitted?: (outcome: Outcome<InventoryAdjustResult>) => void;
  /**
   * The values each selected row fills. One submission then sends one input
   * for each row, in one call, and the form hides the inputs the rows fill.
   */
  readonly rows?: readonly object[];
  /** Called with the outcome of each row's input, in row order. */
  readonly onEach?: (outcomes: readonly Outcome<InventoryAdjustResult>[]) => void;
}

/**
 * The form for `wamn-wms:inventory/adjust@2.0.0`.
 *
 * It renders what the operator fills and nothing else. The reserved inputs
 * come from the runtime at submit time, and the operator never sees them.
 */
export function InventoryAdjustForm(props: InventoryAdjustFormProps) {
  const [refusal, setRefusal] = createSignal<{ text: string; member: string | null } | null>(null);
  const [done, setDone] = createSignal(false);

  const [valuePackagingIdValue, setValuePackagingIdValue] = createSignal<JsonValue | undefined>(
    readMember(props.initial, ["value", "packagingId"]) as JsonValue | undefined,
  );
  const [valueProductIdValue, setValueProductIdValue] = createSignal<JsonValue | undefined>(
    readMember(props.initial, ["value", "productId"]) as JsonValue | undefined,
  );
  const [valueQuantityValue, setValueQuantityValue] = createSignal<JsonValue | undefined>(
    readMember(props.initial, ["value", "quantity"]) as JsonValue | undefined,
  );
  const [valueReasonCodeValue, setValueReasonCodeValue] = createSignal<JsonValue | undefined>(
    readMember(props.initial, ["value", "reasonCode"]) as JsonValue | undefined,
  );
  const [valueStatusValue, setValueStatusValue] = createSignal<JsonValue | undefined>(
    readMember(props.initial, ["value", "status"]) as JsonValue | undefined,
  );

  const submit = async () => {
    setDone(false);
    let value = { ...props.initial } as Partial<InventoryAdjustRequest>;
    const hold = (path: readonly string[], member: JsonValue | undefined) => {
      if (member !== undefined) {
        value = writeMember(value, path, member);
      }
    };
    hold(["value", "packagingId"], valuePackagingIdValue());
    hold(["value", "productId"], valueProductIdValue());
    hold(["value", "quantity"], valueQuantityValue());
    hold(["value", "reasonCode"], valueReasonCodeValue());
    hold(["value", "status"], valueStatusValue());
    if (props.rows !== undefined) {
      const items: InventoryAdjustRequest[] = [];
      for (const row of props.rows) {
        const each = mergeMembers(value, row);
        const checked = ADJUST_INPUT.safeParse(each);
        if (!checked.success) {
          const issue = checked.error.issues[0];
          setRefusal({
            text: issue?.message ?? "A value is not valid.",
            member: checkedMember(
              issue?.path as (string | number)[] | undefined,
              INVENTORY_ADJUST_REQUEST_FIELDS,
            ),
          });
          return;
        }
        let item = { ...each } as InventoryAdjustRequest;
        item = writeMember(item, ["requestId"], newRequestId());
        item = writeMember(item, ["value", "idempotencyKey"], newIdempotencyKey());
        item = writeMember(item, ["value", "occurredAt"], occurredAt());
        if (readMember(item, ["value", "expectedRowVersion"]) === undefined) {
          const valuePackagingIdChosen = valuePackagingIdRevision();
          if (valuePackagingIdChosen === null) {
            setRefusal({ text: "Choose the record from its list.", member: "value.packaging_id" });
            return;
          }
          item = writeMember(item, ["value", "expectedRowVersion"], valuePackagingIdChosen);
        }
        items.push(item);
      }
      props.onEach?.(
        await callEach<InventoryAdjustResult>(
          props.transport,
          { route: INVENTORY_ADJUST_ROUTE, request: INVENTORY_ADJUST_REQUEST_FIELDS, result: INVENTORY_ADJUST_RESULT_FIELDS },
          items,
        ),
      );
      return;
    }
    const checked = ADJUST_INPUT.safeParse(value);
    if (!checked.success) {
      const issue = checked.error.issues[0];
      setRefusal({
        text: issue?.message ?? "A value is not valid.",
        member: checkedMember(
          issue?.path as (string | number)[] | undefined,
          INVENTORY_ADJUST_REQUEST_FIELDS,
        ),
      });
      return;
    }
    let item = { ...value } as InventoryAdjustRequest;
    item = writeMember(item, ["requestId"], newRequestId());
    item = writeMember(item, ["value", "idempotencyKey"], newIdempotencyKey());
    item = writeMember(item, ["value", "occurredAt"], occurredAt());
    const valuePackagingIdChosen = valuePackagingIdRevision();
    if (valuePackagingIdChosen === null) {
      setRefusal({ text: "Choose the record from its list.", member: "value.packaging_id" });
      return;
    }
    item = writeMember(item, ["value", "expectedRowVersion"], valuePackagingIdChosen);
    const outcome = await adjust(props.transport, [item]);
    props.onSubmitted?.(outcome);
    announceOutcome(outcome, InventoryAdjustFormLabel);
    setRefusal(
      outcome.status === "refused"
        ? { text: refusalSentence(outcome.code, outcome.text), member: refusedMember(outcome.detail) }
        : null,
    );
    setDone(outcome.status === "completed");
  };
  const [valuePackagingIdOptions, setValuePackagingIdOptions] = createSignal<PageState<PackagingQueryRow>>(emptyPage<PackagingQueryRow>());
  const [valuePackagingIdSearch, setValuePackagingIdSearch] = createSignal("");
  const [valuePackagingIdRevision, setValuePackagingIdRevision] = createSignal<PackagingQueryRow["rowVersion"] | null>(null);
  const readValuePackagingIdOptions = async (cursor: string | null) => {
    let request = {} as PackagingQueryRequest;
    if (valuePackagingIdSearch() !== "") {
      request = writeMember(request, ["filter", "packagingCode"], [valuePackagingIdSearch()]) as PackagingQueryRequest;
    }
    if (cursor !== null) {
      request = writeMember(request, ["cursor"], cursor) as PackagingQueryRequest;
    }
    const outcome = await packagingQuery(props.transport, [request]);
    if (outcome.status !== "completed") {
      return;
    }
    const rows = outcome.value.item as PackagingQueryRow[];
    setValuePackagingIdOptions(
      cursor === null
        ? firstPage(rows, outcome.value.nextCursor)
        : appendPage(valuePackagingIdOptions(), rows, outcome.value.nextCursor),
    );
  };
  const readValuePackagingIdRecord = async (key: string): Promise<PackagingQueryRow | null> => {
    const request = writeMember({}, ["id"], key) as PackagingGetRequest;
    const outcome = await packagingGet(props.transport, [request]);
    return outcome.status === "completed" ? ((outcome.value ?? null) as unknown as PackagingQueryRow | null) : null;
  };
  void readValuePackagingIdOptions(null);
  onCleanup(afterWrites(props.transport, () => void readValuePackagingIdOptions(null)));
  const [valueProductIdOptions, setValueProductIdOptions] = createSignal<PageState<ProductQueryRow>>(emptyPage<ProductQueryRow>());
  const [valueProductIdSearch, setValueProductIdSearch] = createSignal("");
  const readValueProductIdOptions = async (cursor: string | null) => {
    let request = {} as ProductQueryRequest;
    if (valueProductIdSearch() !== "") {
      request = writeMember(request, ["filter", "productCode"], [valueProductIdSearch()]) as ProductQueryRequest;
    }
    if (cursor !== null) {
      request = writeMember(request, ["cursor"], cursor) as ProductQueryRequest;
    }
    const outcome = await productQuery(props.transport, [request]);
    if (outcome.status !== "completed") {
      return;
    }
    const rows = outcome.value.item as ProductQueryRow[];
    setValueProductIdOptions(
      cursor === null
        ? firstPage(rows, outcome.value.nextCursor)
        : appendPage(valueProductIdOptions(), rows, outcome.value.nextCursor),
    );
  };
  const readValueProductIdRecord = async (key: string): Promise<ProductQueryRow | null> => {
    const request = writeMember({}, ["id"], key) as ProductGetRequest;
    const outcome = await productGet(props.transport, [request]);
    return outcome.status === "completed" ? ((outcome.value ?? null) as unknown as ProductQueryRow | null) : null;
  };
  void readValueProductIdOptions(null);
  onCleanup(afterWrites(props.transport, () => void readValueProductIdOptions(null)));
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
        <Show when={!rowsFill(["value", "packagingId"])}>
          <RecordSelect
            label="packaging id"
            options={valuePackagingIdOptions().rows}
            optionValue={(row) => String(row.id)}
            optionLabel={(row) => String(row.packagingCode)}
            value={valuePackagingIdValue() == null ? null : String(valuePackagingIdValue())}
            onChange={(value) => setValuePackagingIdValue(value ?? "")}
            onRow={(row) => setValuePackagingIdRevision(row?.rowVersion ?? null)}
            onSearch={(text) => {
              setValuePackagingIdSearch(text);
              void readValuePackagingIdOptions(null);
            }}
            hasNextPage={hasNextPage(valuePackagingIdOptions())}
            onNextPage={() => void readValuePackagingIdOptions(valuePackagingIdOptions().cursor)}
            readRow={readValuePackagingIdRecord}
            error={refusalMarks(refusal()?.member ?? null, "value.packaging_id") ? (refusal()?.text ?? null) : null}
          />
        </Show>
        <Show when={!rowsFill(["value", "productId"])}>
          <RecordSelect
            label="product id"
            options={valueProductIdOptions().rows}
            optionValue={(row) => String(row.id)}
            optionLabel={(row) => String(row.productCode)}
            value={valueProductIdValue() == null ? null : String(valueProductIdValue())}
            onChange={(value) => setValueProductIdValue(value ?? "")}
            onSearch={(text) => {
              setValueProductIdSearch(text);
              void readValueProductIdOptions(null);
            }}
            hasNextPage={hasNextPage(valueProductIdOptions())}
            onNextPage={() => void readValueProductIdOptions(valueProductIdOptions().cursor)}
            readRow={readValueProductIdRecord}
            error={refusalMarks(refusal()?.member ?? null, "value.product_id") ? (refusal()?.text ?? null) : null}
          />
        </Show>
        <Show when={!rowsFill(["value", "quantity"])}>
          <TextField
            label="quantity"
            type="text"
            value={String(valueQuantityValue() ?? "")}
            onInput={(value) => setValueQuantityValue(value)}
            error={refusalMarks(refusal()?.member ?? null, "value.quantity") ? (refusal()?.text ?? null) : null}
          />
        </Show>
        <Show when={!rowsFill(["value", "reasonCode"])}>
          <TextField
            label="reason code"
            type="text"
            value={String(valueReasonCodeValue() ?? "")}
            onInput={(value) => setValueReasonCodeValue(value)}
            error={refusalMarks(refusal()?.member ?? null, "value.reason_code") ? (refusal()?.text ?? null) : null}
          />
        </Show>
        <Show when={!rowsFill(["value", "status"])}>
          <ChoiceField
            label="status"
            allowEmpty={false}
            choices={[
              { value: "available", text: "available" },
              { value: "held", text: "held" },
            ]}
            value={String(valueStatusValue() ?? "")}
            onChange={(value) => setValueStatusValue(value as "available" | "held")}
            error={refusalMarks(refusal()?.member ?? null, "value.status") ? (refusal()?.text ?? null) : null}
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

/** What the table for `wamn-wms:inventory/aggregate@2.0.0` takes. */
export interface InventoryAggregateTableProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** Input the parent fixes, which the operator does not edit. */
  readonly fixed?: Partial<InventoryAggregateRequest>;
  /** For each operation whose record a row opens, what opening it does. A row shows a button only for these. */
  readonly onOpen?: { readonly [operation: string]: (row: InventoryAggregateRow) => void };
  /** For each operation whose form a row fills, what the filled values do. */
  readonly onFill?: { readonly [operation: string]: (initial: object) => void };
  /** Called with every outcome this screen reads. */
  readonly onOutcome?: (outcome: Outcome<InventoryAggregateResult>) => void;
}

/** The table for `wamn-wms:inventory/aggregate@2.0.0`: the QueryTable over `INVENTORY_AGGREGATE_TABLE`, in the table screen. */
export function InventoryAggregateTable(props: InventoryAggregateTableProps) {
  return (
    <TableScreen>
      <QueryTable<InventoryAggregateRow, InventoryAggregateResult> definition={INVENTORY_AGGREGATE_TABLE} label={InventoryAggregateTableLabel} {...props} />
    </TableScreen>
  );
}

/** The table definition of `wamn-wms:inventory/aggregate@2.0.0`. */
export const INVENTORY_AGGREGATE_TABLE = {
  name: "inventory",
  read: { route: INVENTORY_AGGREGATE_ROUTE, request: INVENTORY_AGGREGATE_REQUEST_FIELDS, result: INVENTORY_AGGREGATE_RESULT_FIELDS },
  rows: "rows",
  rowId: ["productId", "locationId", "status"],
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
    { field: "locationId", label: "location id", type: "uuid", role: "key" },
    { field: "packagingCount", label: "packaging count", type: "int32", role: "value" },
    { field: "productId", label: "product id", type: "uuid", role: "key" },
    { field: "quantity", label: "quantity", type: "numeric", role: "value" },
    { field: "status", label: "status", type: "text", role: "key" },
  ],
  actions: [],
  childTables: [],
} as const;

/** What an operator types for `wamn-wms:inventory/merge@2.0.0`. */
const MERGE_INPUT = z.object({
  value: z.optional(
    z.object({
      sourcePackagingId: z.string().check(z.regex(UUID_TEXT, "expected a UUID")),
      targetPackagingId: z.string().check(z.regex(UUID_TEXT, "expected a UUID")),
    }),
  ),
});

/** What the form for `wamn-wms:inventory/merge@2.0.0` can start with. */
export interface InventoryMergeFormInitial {
  value?: {
    sourcePackagingId?: Uuid;
    targetPackagingId?: Uuid;
  };
}

/** What the form for `wamn-wms:inventory/merge@2.0.0` takes. */
export interface InventoryMergeFormProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** Values the form starts with. */
  readonly initial?: InventoryMergeFormInitial;
  /** Called with the outcome of every submission. */
  readonly onSubmitted?: (outcome: Outcome<InventoryMergeResult>) => void;
  /**
   * The values each selected row fills. One submission then sends one input
   * for each row, in one call, and the form hides the inputs the rows fill.
   */
  readonly rows?: readonly object[];
  /** Called with the outcome of each row's input, in row order. */
  readonly onEach?: (outcomes: readonly Outcome<InventoryMergeResult>[]) => void;
}

/**
 * The form for `wamn-wms:inventory/merge@2.0.0`.
 *
 * It renders what the operator fills and nothing else. The reserved inputs
 * come from the runtime at submit time, and the operator never sees them.
 */
export function InventoryMergeForm(props: InventoryMergeFormProps) {
  const [refusal, setRefusal] = createSignal<{ text: string; member: string | null } | null>(null);
  const [done, setDone] = createSignal(false);

  const [valueSourcePackagingIdValue, setValueSourcePackagingIdValue] = createSignal<JsonValue | undefined>(
    readMember(props.initial, ["value", "sourcePackagingId"]) as JsonValue | undefined,
  );
  const [valueTargetPackagingIdValue, setValueTargetPackagingIdValue] = createSignal<JsonValue | undefined>(
    readMember(props.initial, ["value", "targetPackagingId"]) as JsonValue | undefined,
  );

  const submit = async () => {
    setDone(false);
    let value = { ...props.initial } as Partial<InventoryMergeRequest>;
    const hold = (path: readonly string[], member: JsonValue | undefined) => {
      if (member !== undefined) {
        value = writeMember(value, path, member);
      }
    };
    hold(["value", "sourcePackagingId"], valueSourcePackagingIdValue());
    hold(["value", "targetPackagingId"], valueTargetPackagingIdValue());
    if (props.rows !== undefined) {
      const items: InventoryMergeRequest[] = [];
      for (const row of props.rows) {
        const each = mergeMembers(value, row);
        const checked = MERGE_INPUT.safeParse(each);
        if (!checked.success) {
          const issue = checked.error.issues[0];
          setRefusal({
            text: issue?.message ?? "A value is not valid.",
            member: checkedMember(
              issue?.path as (string | number)[] | undefined,
              INVENTORY_MERGE_REQUEST_FIELDS,
            ),
          });
          return;
        }
        let item = { ...each } as InventoryMergeRequest;
        item = writeMember(item, ["requestId"], newRequestId());
        item = writeMember(item, ["value", "idempotencyKey"], newIdempotencyKey());
        item = writeMember(item, ["value", "occurredAt"], occurredAt());
        if (readMember(item, ["value", "expectedRowVersion"]) === undefined) {
          const valueTargetPackagingIdChosen = valueTargetPackagingIdRevision();
          if (valueTargetPackagingIdChosen === null) {
            setRefusal({ text: "Choose the record from its list.", member: "value.target_packaging_id" });
            return;
          }
          item = writeMember(item, ["value", "expectedRowVersion"], valueTargetPackagingIdChosen);
        }
        items.push(item);
      }
      props.onEach?.(
        await callEach<InventoryMergeResult>(
          props.transport,
          { route: INVENTORY_MERGE_ROUTE, request: INVENTORY_MERGE_REQUEST_FIELDS, result: INVENTORY_MERGE_RESULT_FIELDS },
          items,
        ),
      );
      return;
    }
    const checked = MERGE_INPUT.safeParse(value);
    if (!checked.success) {
      const issue = checked.error.issues[0];
      setRefusal({
        text: issue?.message ?? "A value is not valid.",
        member: checkedMember(
          issue?.path as (string | number)[] | undefined,
          INVENTORY_MERGE_REQUEST_FIELDS,
        ),
      });
      return;
    }
    let item = { ...value } as InventoryMergeRequest;
    item = writeMember(item, ["requestId"], newRequestId());
    item = writeMember(item, ["value", "idempotencyKey"], newIdempotencyKey());
    item = writeMember(item, ["value", "occurredAt"], occurredAt());
    const valueTargetPackagingIdChosen = valueTargetPackagingIdRevision();
    if (valueTargetPackagingIdChosen === null) {
      setRefusal({ text: "Choose the record from its list.", member: "value.target_packaging_id" });
      return;
    }
    item = writeMember(item, ["value", "expectedRowVersion"], valueTargetPackagingIdChosen);
    const outcome = await merge(props.transport, [item]);
    props.onSubmitted?.(outcome);
    announceOutcome(outcome, InventoryMergeFormLabel);
    setRefusal(
      outcome.status === "refused"
        ? { text: refusalSentence(outcome.code, outcome.text), member: refusedMember(outcome.detail) }
        : null,
    );
    setDone(outcome.status === "completed");
  };
  const [valueSourcePackagingIdOptions, setValueSourcePackagingIdOptions] = createSignal<PageState<PackagingQueryRow>>(emptyPage<PackagingQueryRow>());
  const [valueSourcePackagingIdSearch, setValueSourcePackagingIdSearch] = createSignal("");
  const readValueSourcePackagingIdOptions = async (cursor: string | null) => {
    let request = {} as PackagingQueryRequest;
    if (valueSourcePackagingIdSearch() !== "") {
      request = writeMember(request, ["filter", "packagingCode"], [valueSourcePackagingIdSearch()]) as PackagingQueryRequest;
    }
    if (cursor !== null) {
      request = writeMember(request, ["cursor"], cursor) as PackagingQueryRequest;
    }
    const outcome = await packagingQuery(props.transport, [request]);
    if (outcome.status !== "completed") {
      return;
    }
    const rows = outcome.value.item as PackagingQueryRow[];
    setValueSourcePackagingIdOptions(
      cursor === null
        ? firstPage(rows, outcome.value.nextCursor)
        : appendPage(valueSourcePackagingIdOptions(), rows, outcome.value.nextCursor),
    );
  };
  const readValueSourcePackagingIdRecord = async (key: string): Promise<PackagingQueryRow | null> => {
    const request = writeMember({}, ["id"], key) as PackagingGetRequest;
    const outcome = await packagingGet(props.transport, [request]);
    return outcome.status === "completed" ? ((outcome.value ?? null) as unknown as PackagingQueryRow | null) : null;
  };
  void readValueSourcePackagingIdOptions(null);
  onCleanup(afterWrites(props.transport, () => void readValueSourcePackagingIdOptions(null)));
  const [valueTargetPackagingIdOptions, setValueTargetPackagingIdOptions] = createSignal<PageState<PackagingQueryRow>>(emptyPage<PackagingQueryRow>());
  const [valueTargetPackagingIdSearch, setValueTargetPackagingIdSearch] = createSignal("");
  const [valueTargetPackagingIdRevision, setValueTargetPackagingIdRevision] = createSignal<PackagingQueryRow["rowVersion"] | null>(null);
  const readValueTargetPackagingIdOptions = async (cursor: string | null) => {
    let request = {} as PackagingQueryRequest;
    if (valueTargetPackagingIdSearch() !== "") {
      request = writeMember(request, ["filter", "packagingCode"], [valueTargetPackagingIdSearch()]) as PackagingQueryRequest;
    }
    if (cursor !== null) {
      request = writeMember(request, ["cursor"], cursor) as PackagingQueryRequest;
    }
    const outcome = await packagingQuery(props.transport, [request]);
    if (outcome.status !== "completed") {
      return;
    }
    const rows = outcome.value.item as PackagingQueryRow[];
    setValueTargetPackagingIdOptions(
      cursor === null
        ? firstPage(rows, outcome.value.nextCursor)
        : appendPage(valueTargetPackagingIdOptions(), rows, outcome.value.nextCursor),
    );
  };
  const readValueTargetPackagingIdRecord = async (key: string): Promise<PackagingQueryRow | null> => {
    const request = writeMember({}, ["id"], key) as PackagingGetRequest;
    const outcome = await packagingGet(props.transport, [request]);
    return outcome.status === "completed" ? ((outcome.value ?? null) as unknown as PackagingQueryRow | null) : null;
  };
  void readValueTargetPackagingIdOptions(null);
  onCleanup(afterWrites(props.transport, () => void readValueTargetPackagingIdOptions(null)));
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
        <Show when={!rowsFill(["value", "sourcePackagingId"])}>
          <RecordSelect
            label="source packaging id"
            options={valueSourcePackagingIdOptions().rows}
            optionValue={(row) => String(row.id)}
            optionLabel={(row) => String(row.packagingCode)}
            value={valueSourcePackagingIdValue() == null ? null : String(valueSourcePackagingIdValue())}
            onChange={(value) => setValueSourcePackagingIdValue(value ?? "")}
            onSearch={(text) => {
              setValueSourcePackagingIdSearch(text);
              void readValueSourcePackagingIdOptions(null);
            }}
            hasNextPage={hasNextPage(valueSourcePackagingIdOptions())}
            onNextPage={() => void readValueSourcePackagingIdOptions(valueSourcePackagingIdOptions().cursor)}
            readRow={readValueSourcePackagingIdRecord}
            error={refusalMarks(refusal()?.member ?? null, "value.source_packaging_id") ? (refusal()?.text ?? null) : null}
          />
        </Show>
        <Show when={!rowsFill(["value", "targetPackagingId"])}>
          <RecordSelect
            label="target packaging id"
            options={valueTargetPackagingIdOptions().rows}
            optionValue={(row) => String(row.id)}
            optionLabel={(row) => String(row.packagingCode)}
            value={valueTargetPackagingIdValue() == null ? null : String(valueTargetPackagingIdValue())}
            onChange={(value) => setValueTargetPackagingIdValue(value ?? "")}
            onRow={(row) => setValueTargetPackagingIdRevision(row?.rowVersion ?? null)}
            onSearch={(text) => {
              setValueTargetPackagingIdSearch(text);
              void readValueTargetPackagingIdOptions(null);
            }}
            hasNextPage={hasNextPage(valueTargetPackagingIdOptions())}
            onNextPage={() => void readValueTargetPackagingIdOptions(valueTargetPackagingIdOptions().cursor)}
            readRow={readValueTargetPackagingIdRecord}
            error={refusalMarks(refusal()?.member ?? null, "value.target_packaging_id") ? (refusal()?.text ?? null) : null}
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

/** What an operator types for `wamn-wms:inventory/move@2.0.0`. */
const MOVE_INPUT = z.object({
  value: z.optional(
    z.object({
      packagingId: z.string().check(z.regex(UUID_TEXT, "expected a UUID")),
      toLocationId: z.string().check(z.regex(UUID_TEXT, "expected a UUID")),
    }),
  ),
});

/** What the form for `wamn-wms:inventory/move@2.0.0` can start with. */
export interface InventoryMoveFormInitial {
  value?: {
    packagingId?: Uuid;
    toLocationId?: Uuid;
  };
}

/** What the form for `wamn-wms:inventory/move@2.0.0` takes. */
export interface InventoryMoveFormProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** Values the form starts with. */
  readonly initial?: InventoryMoveFormInitial;
  /** Called with the outcome of every submission. */
  readonly onSubmitted?: (outcome: Outcome<InventoryMoveResult>) => void;
  /**
   * The values each selected row fills. One submission then sends one input
   * for each row, in one call, and the form hides the inputs the rows fill.
   */
  readonly rows?: readonly object[];
  /** Called with the outcome of each row's input, in row order. */
  readonly onEach?: (outcomes: readonly Outcome<InventoryMoveResult>[]) => void;
}

/**
 * The form for `wamn-wms:inventory/move@2.0.0`.
 *
 * It renders what the operator fills and nothing else. The reserved inputs
 * come from the runtime at submit time, and the operator never sees them.
 */
export function InventoryMoveForm(props: InventoryMoveFormProps) {
  const [refusal, setRefusal] = createSignal<{ text: string; member: string | null } | null>(null);
  const [done, setDone] = createSignal(false);

  const [valuePackagingIdValue, setValuePackagingIdValue] = createSignal<JsonValue | undefined>(
    readMember(props.initial, ["value", "packagingId"]) as JsonValue | undefined,
  );
  const [valueToLocationIdValue, setValueToLocationIdValue] = createSignal<JsonValue | undefined>(
    readMember(props.initial, ["value", "toLocationId"]) as JsonValue | undefined,
  );

  const submit = async () => {
    setDone(false);
    let value = { ...props.initial } as Partial<InventoryMoveRequest>;
    const hold = (path: readonly string[], member: JsonValue | undefined) => {
      if (member !== undefined) {
        value = writeMember(value, path, member);
      }
    };
    hold(["value", "packagingId"], valuePackagingIdValue());
    hold(["value", "toLocationId"], valueToLocationIdValue());
    if (props.rows !== undefined) {
      const items: InventoryMoveRequest[] = [];
      for (const row of props.rows) {
        const each = mergeMembers(value, row);
        const checked = MOVE_INPUT.safeParse(each);
        if (!checked.success) {
          const issue = checked.error.issues[0];
          setRefusal({
            text: issue?.message ?? "A value is not valid.",
            member: checkedMember(
              issue?.path as (string | number)[] | undefined,
              INVENTORY_MOVE_REQUEST_FIELDS,
            ),
          });
          return;
        }
        let item = { ...each } as InventoryMoveRequest;
        item = writeMember(item, ["requestId"], newRequestId());
        item = writeMember(item, ["value", "idempotencyKey"], newIdempotencyKey());
        item = writeMember(item, ["value", "occurredAt"], occurredAt());
        if (readMember(item, ["value", "expectedRowVersion"]) === undefined) {
          const valuePackagingIdChosen = valuePackagingIdRevision();
          if (valuePackagingIdChosen === null) {
            setRefusal({ text: "Choose the record from its list.", member: "value.packaging_id" });
            return;
          }
          item = writeMember(item, ["value", "expectedRowVersion"], valuePackagingIdChosen);
        }
        items.push(item);
      }
      props.onEach?.(
        await callEach<InventoryMoveResult>(
          props.transport,
          { route: INVENTORY_MOVE_ROUTE, request: INVENTORY_MOVE_REQUEST_FIELDS, result: INVENTORY_MOVE_RESULT_FIELDS },
          items,
        ),
      );
      return;
    }
    const checked = MOVE_INPUT.safeParse(value);
    if (!checked.success) {
      const issue = checked.error.issues[0];
      setRefusal({
        text: issue?.message ?? "A value is not valid.",
        member: checkedMember(
          issue?.path as (string | number)[] | undefined,
          INVENTORY_MOVE_REQUEST_FIELDS,
        ),
      });
      return;
    }
    let item = { ...value } as InventoryMoveRequest;
    item = writeMember(item, ["requestId"], newRequestId());
    item = writeMember(item, ["value", "idempotencyKey"], newIdempotencyKey());
    item = writeMember(item, ["value", "occurredAt"], occurredAt());
    const valuePackagingIdChosen = valuePackagingIdRevision();
    if (valuePackagingIdChosen === null) {
      setRefusal({ text: "Choose the record from its list.", member: "value.packaging_id" });
      return;
    }
    item = writeMember(item, ["value", "expectedRowVersion"], valuePackagingIdChosen);
    const outcome = await move(props.transport, [item]);
    props.onSubmitted?.(outcome);
    announceOutcome(outcome, InventoryMoveFormLabel);
    setRefusal(
      outcome.status === "refused"
        ? { text: refusalSentence(outcome.code, outcome.text), member: refusedMember(outcome.detail) }
        : null,
    );
    setDone(outcome.status === "completed");
  };
  const [valuePackagingIdOptions, setValuePackagingIdOptions] = createSignal<PageState<PackagingQueryRow>>(emptyPage<PackagingQueryRow>());
  const [valuePackagingIdSearch, setValuePackagingIdSearch] = createSignal("");
  const [valuePackagingIdRevision, setValuePackagingIdRevision] = createSignal<PackagingQueryRow["rowVersion"] | null>(null);
  const readValuePackagingIdOptions = async (cursor: string | null) => {
    let request = {} as PackagingQueryRequest;
    if (valuePackagingIdSearch() !== "") {
      request = writeMember(request, ["filter", "packagingCode"], [valuePackagingIdSearch()]) as PackagingQueryRequest;
    }
    if (cursor !== null) {
      request = writeMember(request, ["cursor"], cursor) as PackagingQueryRequest;
    }
    const outcome = await packagingQuery(props.transport, [request]);
    if (outcome.status !== "completed") {
      return;
    }
    const rows = outcome.value.item as PackagingQueryRow[];
    setValuePackagingIdOptions(
      cursor === null
        ? firstPage(rows, outcome.value.nextCursor)
        : appendPage(valuePackagingIdOptions(), rows, outcome.value.nextCursor),
    );
  };
  const readValuePackagingIdRecord = async (key: string): Promise<PackagingQueryRow | null> => {
    const request = writeMember({}, ["id"], key) as PackagingGetRequest;
    const outcome = await packagingGet(props.transport, [request]);
    return outcome.status === "completed" ? ((outcome.value ?? null) as unknown as PackagingQueryRow | null) : null;
  };
  void readValuePackagingIdOptions(null);
  onCleanup(afterWrites(props.transport, () => void readValuePackagingIdOptions(null)));
  const [valueToLocationIdOptions, setValueToLocationIdOptions] = createSignal<PageState<LocationQueryRow>>(emptyPage<LocationQueryRow>());
  const [valueToLocationIdSearch, setValueToLocationIdSearch] = createSignal("");
  const readValueToLocationIdOptions = async (cursor: string | null) => {
    let request = {} as LocationQueryRequest;
    if (valueToLocationIdSearch() !== "") {
      request = writeMember(request, ["filter", "locationCode"], [valueToLocationIdSearch()]) as LocationQueryRequest;
    }
    if (cursor !== null) {
      request = writeMember(request, ["cursor"], cursor) as LocationQueryRequest;
    }
    const outcome = await locationQuery(props.transport, [request]);
    if (outcome.status !== "completed") {
      return;
    }
    const rows = outcome.value.item as LocationQueryRow[];
    setValueToLocationIdOptions(
      cursor === null
        ? firstPage(rows, outcome.value.nextCursor)
        : appendPage(valueToLocationIdOptions(), rows, outcome.value.nextCursor),
    );
  };
  const readValueToLocationIdRecord = async (key: string): Promise<LocationQueryRow | null> => {
    const request = writeMember({}, ["id"], key) as LocationGetRequest;
    const outcome = await locationGet(props.transport, [request]);
    return outcome.status === "completed" ? ((outcome.value ?? null) as unknown as LocationQueryRow | null) : null;
  };
  void readValueToLocationIdOptions(null);
  onCleanup(afterWrites(props.transport, () => void readValueToLocationIdOptions(null)));
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
        <Show when={!rowsFill(["value", "packagingId"])}>
          <RecordSelect
            label="packaging id"
            options={valuePackagingIdOptions().rows}
            optionValue={(row) => String(row.id)}
            optionLabel={(row) => String(row.packagingCode)}
            value={valuePackagingIdValue() == null ? null : String(valuePackagingIdValue())}
            onChange={(value) => setValuePackagingIdValue(value ?? "")}
            onRow={(row) => setValuePackagingIdRevision(row?.rowVersion ?? null)}
            onSearch={(text) => {
              setValuePackagingIdSearch(text);
              void readValuePackagingIdOptions(null);
            }}
            hasNextPage={hasNextPage(valuePackagingIdOptions())}
            onNextPage={() => void readValuePackagingIdOptions(valuePackagingIdOptions().cursor)}
            readRow={readValuePackagingIdRecord}
            error={refusalMarks(refusal()?.member ?? null, "value.packaging_id") ? (refusal()?.text ?? null) : null}
          />
        </Show>
        <Show when={!rowsFill(["value", "toLocationId"])}>
          <RecordSelect
            label="to location id"
            options={valueToLocationIdOptions().rows}
            optionValue={(row) => String(row.id)}
            optionLabel={(row) => String(row.locationCode)}
            value={valueToLocationIdValue() == null ? null : String(valueToLocationIdValue())}
            onChange={(value) => setValueToLocationIdValue(value ?? "")}
            onSearch={(text) => {
              setValueToLocationIdSearch(text);
              void readValueToLocationIdOptions(null);
            }}
            hasNextPage={hasNextPage(valueToLocationIdOptions())}
            onNextPage={() => void readValueToLocationIdOptions(valueToLocationIdOptions().cursor)}
            readRow={readValueToLocationIdRecord}
            error={refusalMarks(refusal()?.member ?? null, "value.to_location_id") ? (refusal()?.text ?? null) : null}
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

/** What an operator types for `wamn-wms:inventory/split@2.0.0`. */
const SPLIT_INPUT = z.object({
  value: z.optional(
    z.object({
      newPackagingCode: z.string(),
      newPackagingType: z.enum(["bin", "case", "loose", "pallet", "tote"]),
      productId: z.string().check(z.regex(UUID_TEXT, "expected a UUID")),
      quantity: z.string().check(z.regex(NUMERIC_TEXT, "expected decimal text")),
      sourcePackagingId: z.string().check(z.regex(UUID_TEXT, "expected a UUID")),
      status: z.enum(["available", "held"]),
      toLocationId: z.string().check(z.regex(UUID_TEXT, "expected a UUID")),
    }),
  ),
});

/** What the form for `wamn-wms:inventory/split@2.0.0` can start with. */
export interface InventorySplitFormInitial {
  value?: {
    newPackagingCode?: string;
    newPackagingType?: "bin" | "case" | "loose" | "pallet" | "tote";
    productId?: Uuid;
    quantity?: Numeric;
    sourcePackagingId?: Uuid;
    status?: "available" | "held";
    toLocationId?: Uuid;
  };
}

/** What the form for `wamn-wms:inventory/split@2.0.0` takes. */
export interface InventorySplitFormProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** Values the form starts with. */
  readonly initial?: InventorySplitFormInitial;
  /** Called with the outcome of every submission. */
  readonly onSubmitted?: (outcome: Outcome<InventorySplitResult>) => void;
  /**
   * The values each selected row fills. One submission then sends one input
   * for each row, in one call, and the form hides the inputs the rows fill.
   */
  readonly rows?: readonly object[];
  /** Called with the outcome of each row's input, in row order. */
  readonly onEach?: (outcomes: readonly Outcome<InventorySplitResult>[]) => void;
}

/**
 * The form for `wamn-wms:inventory/split@2.0.0`.
 *
 * It renders what the operator fills and nothing else. The reserved inputs
 * come from the runtime at submit time, and the operator never sees them.
 */
export function InventorySplitForm(props: InventorySplitFormProps) {
  const [refusal, setRefusal] = createSignal<{ text: string; member: string | null } | null>(null);
  const [done, setDone] = createSignal(false);

  const [valueNewPackagingCodeValue, setValueNewPackagingCodeValue] = createSignal<JsonValue | undefined>(
    readMember(props.initial, ["value", "newPackagingCode"]) as JsonValue | undefined,
  );
  const [valueNewPackagingTypeValue, setValueNewPackagingTypeValue] = createSignal<JsonValue | undefined>(
    readMember(props.initial, ["value", "newPackagingType"]) as JsonValue | undefined,
  );
  const [valueProductIdValue, setValueProductIdValue] = createSignal<JsonValue | undefined>(
    readMember(props.initial, ["value", "productId"]) as JsonValue | undefined,
  );
  const [valueQuantityValue, setValueQuantityValue] = createSignal<JsonValue | undefined>(
    readMember(props.initial, ["value", "quantity"]) as JsonValue | undefined,
  );
  const [valueSourcePackagingIdValue, setValueSourcePackagingIdValue] = createSignal<JsonValue | undefined>(
    readMember(props.initial, ["value", "sourcePackagingId"]) as JsonValue | undefined,
  );
  const [valueStatusValue, setValueStatusValue] = createSignal<JsonValue | undefined>(
    readMember(props.initial, ["value", "status"]) as JsonValue | undefined,
  );
  const [valueToLocationIdValue, setValueToLocationIdValue] = createSignal<JsonValue | undefined>(
    readMember(props.initial, ["value", "toLocationId"]) as JsonValue | undefined,
  );

  const submit = async () => {
    setDone(false);
    let value = { ...props.initial } as Partial<InventorySplitRequest>;
    const hold = (path: readonly string[], member: JsonValue | undefined) => {
      if (member !== undefined) {
        value = writeMember(value, path, member);
      }
    };
    hold(["value", "newPackagingCode"], valueNewPackagingCodeValue());
    hold(["value", "newPackagingType"], valueNewPackagingTypeValue());
    hold(["value", "productId"], valueProductIdValue());
    hold(["value", "quantity"], valueQuantityValue());
    hold(["value", "sourcePackagingId"], valueSourcePackagingIdValue());
    hold(["value", "status"], valueStatusValue());
    hold(["value", "toLocationId"], valueToLocationIdValue());
    if (props.rows !== undefined) {
      const items: InventorySplitRequest[] = [];
      for (const row of props.rows) {
        const each = mergeMembers(value, row);
        const checked = SPLIT_INPUT.safeParse(each);
        if (!checked.success) {
          const issue = checked.error.issues[0];
          setRefusal({
            text: issue?.message ?? "A value is not valid.",
            member: checkedMember(
              issue?.path as (string | number)[] | undefined,
              INVENTORY_SPLIT_REQUEST_FIELDS,
            ),
          });
          return;
        }
        let item = { ...each } as InventorySplitRequest;
        item = writeMember(item, ["requestId"], newRequestId());
        item = writeMember(item, ["value", "idempotencyKey"], newIdempotencyKey());
        item = writeMember(item, ["value", "occurredAt"], occurredAt());
        if (readMember(item, ["value", "expectedRowVersion"]) === undefined) {
          const valueSourcePackagingIdChosen = valueSourcePackagingIdRevision();
          if (valueSourcePackagingIdChosen === null) {
            setRefusal({ text: "Choose the record from its list.", member: "value.source_packaging_id" });
            return;
          }
          item = writeMember(item, ["value", "expectedRowVersion"], valueSourcePackagingIdChosen);
        }
        items.push(item);
      }
      props.onEach?.(
        await callEach<InventorySplitResult>(
          props.transport,
          { route: INVENTORY_SPLIT_ROUTE, request: INVENTORY_SPLIT_REQUEST_FIELDS, result: INVENTORY_SPLIT_RESULT_FIELDS },
          items,
        ),
      );
      return;
    }
    const checked = SPLIT_INPUT.safeParse(value);
    if (!checked.success) {
      const issue = checked.error.issues[0];
      setRefusal({
        text: issue?.message ?? "A value is not valid.",
        member: checkedMember(
          issue?.path as (string | number)[] | undefined,
          INVENTORY_SPLIT_REQUEST_FIELDS,
        ),
      });
      return;
    }
    let item = { ...value } as InventorySplitRequest;
    item = writeMember(item, ["requestId"], newRequestId());
    item = writeMember(item, ["value", "idempotencyKey"], newIdempotencyKey());
    item = writeMember(item, ["value", "occurredAt"], occurredAt());
    const valueSourcePackagingIdChosen = valueSourcePackagingIdRevision();
    if (valueSourcePackagingIdChosen === null) {
      setRefusal({ text: "Choose the record from its list.", member: "value.source_packaging_id" });
      return;
    }
    item = writeMember(item, ["value", "expectedRowVersion"], valueSourcePackagingIdChosen);
    const outcome = await split(props.transport, [item]);
    props.onSubmitted?.(outcome);
    announceOutcome(outcome, InventorySplitFormLabel);
    setRefusal(
      outcome.status === "refused"
        ? { text: refusalSentence(outcome.code, outcome.text), member: refusedMember(outcome.detail) }
        : null,
    );
    setDone(outcome.status === "completed");
  };
  const [valueProductIdOptions, setValueProductIdOptions] = createSignal<PageState<ProductQueryRow>>(emptyPage<ProductQueryRow>());
  const [valueProductIdSearch, setValueProductIdSearch] = createSignal("");
  const readValueProductIdOptions = async (cursor: string | null) => {
    let request = {} as ProductQueryRequest;
    if (valueProductIdSearch() !== "") {
      request = writeMember(request, ["filter", "productCode"], [valueProductIdSearch()]) as ProductQueryRequest;
    }
    if (cursor !== null) {
      request = writeMember(request, ["cursor"], cursor) as ProductQueryRequest;
    }
    const outcome = await productQuery(props.transport, [request]);
    if (outcome.status !== "completed") {
      return;
    }
    const rows = outcome.value.item as ProductQueryRow[];
    setValueProductIdOptions(
      cursor === null
        ? firstPage(rows, outcome.value.nextCursor)
        : appendPage(valueProductIdOptions(), rows, outcome.value.nextCursor),
    );
  };
  const readValueProductIdRecord = async (key: string): Promise<ProductQueryRow | null> => {
    const request = writeMember({}, ["id"], key) as ProductGetRequest;
    const outcome = await productGet(props.transport, [request]);
    return outcome.status === "completed" ? ((outcome.value ?? null) as unknown as ProductQueryRow | null) : null;
  };
  void readValueProductIdOptions(null);
  onCleanup(afterWrites(props.transport, () => void readValueProductIdOptions(null)));
  const [valueSourcePackagingIdOptions, setValueSourcePackagingIdOptions] = createSignal<PageState<PackagingQueryRow>>(emptyPage<PackagingQueryRow>());
  const [valueSourcePackagingIdSearch, setValueSourcePackagingIdSearch] = createSignal("");
  const [valueSourcePackagingIdRevision, setValueSourcePackagingIdRevision] = createSignal<PackagingQueryRow["rowVersion"] | null>(null);
  const readValueSourcePackagingIdOptions = async (cursor: string | null) => {
    let request = {} as PackagingQueryRequest;
    if (valueSourcePackagingIdSearch() !== "") {
      request = writeMember(request, ["filter", "packagingCode"], [valueSourcePackagingIdSearch()]) as PackagingQueryRequest;
    }
    if (cursor !== null) {
      request = writeMember(request, ["cursor"], cursor) as PackagingQueryRequest;
    }
    const outcome = await packagingQuery(props.transport, [request]);
    if (outcome.status !== "completed") {
      return;
    }
    const rows = outcome.value.item as PackagingQueryRow[];
    setValueSourcePackagingIdOptions(
      cursor === null
        ? firstPage(rows, outcome.value.nextCursor)
        : appendPage(valueSourcePackagingIdOptions(), rows, outcome.value.nextCursor),
    );
  };
  const readValueSourcePackagingIdRecord = async (key: string): Promise<PackagingQueryRow | null> => {
    const request = writeMember({}, ["id"], key) as PackagingGetRequest;
    const outcome = await packagingGet(props.transport, [request]);
    return outcome.status === "completed" ? ((outcome.value ?? null) as unknown as PackagingQueryRow | null) : null;
  };
  void readValueSourcePackagingIdOptions(null);
  onCleanup(afterWrites(props.transport, () => void readValueSourcePackagingIdOptions(null)));
  const [valueToLocationIdOptions, setValueToLocationIdOptions] = createSignal<PageState<LocationQueryRow>>(emptyPage<LocationQueryRow>());
  const [valueToLocationIdSearch, setValueToLocationIdSearch] = createSignal("");
  const readValueToLocationIdOptions = async (cursor: string | null) => {
    let request = {} as LocationQueryRequest;
    if (valueToLocationIdSearch() !== "") {
      request = writeMember(request, ["filter", "locationCode"], [valueToLocationIdSearch()]) as LocationQueryRequest;
    }
    if (cursor !== null) {
      request = writeMember(request, ["cursor"], cursor) as LocationQueryRequest;
    }
    const outcome = await locationQuery(props.transport, [request]);
    if (outcome.status !== "completed") {
      return;
    }
    const rows = outcome.value.item as LocationQueryRow[];
    setValueToLocationIdOptions(
      cursor === null
        ? firstPage(rows, outcome.value.nextCursor)
        : appendPage(valueToLocationIdOptions(), rows, outcome.value.nextCursor),
    );
  };
  const readValueToLocationIdRecord = async (key: string): Promise<LocationQueryRow | null> => {
    const request = writeMember({}, ["id"], key) as LocationGetRequest;
    const outcome = await locationGet(props.transport, [request]);
    return outcome.status === "completed" ? ((outcome.value ?? null) as unknown as LocationQueryRow | null) : null;
  };
  void readValueToLocationIdOptions(null);
  onCleanup(afterWrites(props.transport, () => void readValueToLocationIdOptions(null)));
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
        <Show when={!rowsFill(["value", "newPackagingCode"])}>
          <TextField
            label="new packaging code"
            type="text"
            value={String(valueNewPackagingCodeValue() ?? "")}
            onInput={(value) => setValueNewPackagingCodeValue(value)}
            error={refusalMarks(refusal()?.member ?? null, "value.new_packaging_code") ? (refusal()?.text ?? null) : null}
          />
        </Show>
        <Show when={!rowsFill(["value", "newPackagingType"])}>
          <ChoiceField
            label="new packaging type"
            allowEmpty={false}
            choices={[
              { value: "bin", text: "bin" },
              { value: "case", text: "case" },
              { value: "loose", text: "loose" },
              { value: "pallet", text: "pallet" },
              { value: "tote", text: "tote" },
            ]}
            value={String(valueNewPackagingTypeValue() ?? "")}
            onChange={(value) => setValueNewPackagingTypeValue(value as "bin" | "case" | "loose" | "pallet" | "tote")}
            error={refusalMarks(refusal()?.member ?? null, "value.new_packaging_type") ? (refusal()?.text ?? null) : null}
          />
        </Show>
        <Show when={!rowsFill(["value", "productId"])}>
          <RecordSelect
            label="product id"
            options={valueProductIdOptions().rows}
            optionValue={(row) => String(row.id)}
            optionLabel={(row) => String(row.productCode)}
            value={valueProductIdValue() == null ? null : String(valueProductIdValue())}
            onChange={(value) => setValueProductIdValue(value ?? "")}
            onSearch={(text) => {
              setValueProductIdSearch(text);
              void readValueProductIdOptions(null);
            }}
            hasNextPage={hasNextPage(valueProductIdOptions())}
            onNextPage={() => void readValueProductIdOptions(valueProductIdOptions().cursor)}
            readRow={readValueProductIdRecord}
            error={refusalMarks(refusal()?.member ?? null, "value.product_id") ? (refusal()?.text ?? null) : null}
          />
        </Show>
        <Show when={!rowsFill(["value", "quantity"])}>
          <TextField
            label="quantity"
            type="text"
            value={String(valueQuantityValue() ?? "")}
            onInput={(value) => setValueQuantityValue(value)}
            error={refusalMarks(refusal()?.member ?? null, "value.quantity") ? (refusal()?.text ?? null) : null}
          />
        </Show>
        <Show when={!rowsFill(["value", "sourcePackagingId"])}>
          <RecordSelect
            label="source packaging id"
            options={valueSourcePackagingIdOptions().rows}
            optionValue={(row) => String(row.id)}
            optionLabel={(row) => String(row.packagingCode)}
            value={valueSourcePackagingIdValue() == null ? null : String(valueSourcePackagingIdValue())}
            onChange={(value) => setValueSourcePackagingIdValue(value ?? "")}
            onRow={(row) => setValueSourcePackagingIdRevision(row?.rowVersion ?? null)}
            onSearch={(text) => {
              setValueSourcePackagingIdSearch(text);
              void readValueSourcePackagingIdOptions(null);
            }}
            hasNextPage={hasNextPage(valueSourcePackagingIdOptions())}
            onNextPage={() => void readValueSourcePackagingIdOptions(valueSourcePackagingIdOptions().cursor)}
            readRow={readValueSourcePackagingIdRecord}
            error={refusalMarks(refusal()?.member ?? null, "value.source_packaging_id") ? (refusal()?.text ?? null) : null}
          />
        </Show>
        <Show when={!rowsFill(["value", "status"])}>
          <ChoiceField
            label="status"
            allowEmpty={false}
            choices={[
              { value: "available", text: "available" },
              { value: "held", text: "held" },
            ]}
            value={String(valueStatusValue() ?? "")}
            onChange={(value) => setValueStatusValue(value as "available" | "held")}
            error={refusalMarks(refusal()?.member ?? null, "value.status") ? (refusal()?.text ?? null) : null}
          />
        </Show>
        <Show when={!rowsFill(["value", "toLocationId"])}>
          <RecordSelect
            label="to location id"
            options={valueToLocationIdOptions().rows}
            optionValue={(row) => String(row.id)}
            optionLabel={(row) => String(row.locationCode)}
            value={valueToLocationIdValue() == null ? null : String(valueToLocationIdValue())}
            onChange={(value) => setValueToLocationIdValue(value ?? "")}
            onSearch={(text) => {
              setValueToLocationIdSearch(text);
              void readValueToLocationIdOptions(null);
            }}
            hasNextPage={hasNextPage(valueToLocationIdOptions())}
            onNextPage={() => void readValueToLocationIdOptions(valueToLocationIdOptions().cursor)}
            readRow={readValueToLocationIdRecord}
            error={refusalMarks(refusal()?.member ?? null, "value.to_location_id") ? (refusal()?.text ?? null) : null}
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
