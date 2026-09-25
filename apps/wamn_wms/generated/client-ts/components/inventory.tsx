// @generated from the client-contract IR; do not edit.
//
// `inventory` components. Each one calls the bindings and the runtime, and
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
  occurredAt,
  refusalMarks,
  refusalSentence,
  refusedMember,
  startRead,
  type JsonValue,
  type Numeric,
  type Outcome,
  type PageState,
  type Transport,
  type Uuid,
  writeMember,
} from "@wamn/web-runtime";
import {
  Badge,
  Button,
  ChoiceField,
  DataGrid,
  DataGridContainer,
  DataGridTable,
  FieldError,
  FieldGroup,
  FormActions,
  FormDone,
  RecordSelect,
  TableScreen,
  TextField,
  announceOutcome,
  gridFeatures,
  type GridFeatures,
} from "@wamn/ui";
import {
  INVENTORY_ADJUST_REQUEST_FIELDS,
  INVENTORY_MERGE_REQUEST_FIELDS,
  INVENTORY_MOVE_REQUEST_FIELDS,
  INVENTORY_SPLIT_REQUEST_FIELDS,
  adjust,
  aggregate,
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
  get as locationGet,
  query as locationQuery,
  type LocationGetRequest,
  type LocationQueryRequest,
  type LocationQueryRow,
} from "../location.js";
import {
  get as palletGet,
  query as palletQuery,
  type PalletGetRequest,
  type PalletQueryRequest,
  type PalletQueryRow,
} from "../pallet.js";
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

/** What an operator types for `wamn-wms:inventory/adjust@1.0.0`. */
const ADJUST_INPUT = z.object({
  value: z
    .object({
      palletId: z.string().regex(UUID_TEXT, "expected a UUID"),
      productId: z.string().regex(UUID_TEXT, "expected a UUID"),
      quantity: z.string().regex(NUMERIC_TEXT, "expected decimal text"),
      reasonCode: z.string(),
      status: z.enum(["available", "held"]),
    })
    .optional(),
});

/** What the form for `wamn-wms:inventory/adjust@1.0.0` can start with. */
export interface InventoryAdjustFormInitial {
  value?: {
    palletId?: Uuid;
    productId?: Uuid;
    quantity?: Numeric;
    reasonCode?: string;
    status?: "available" | "held";
  };
}

/** What the form for `wamn-wms:inventory/adjust@1.0.0` takes. */
export interface InventoryAdjustFormProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** Values the form starts with. */
  readonly initial?: InventoryAdjustFormInitial;
  /** The revision this command sends. The release binds no read that supplies it. */
  readonly valueExpectedRowVersion: InventoryAdjustRequest["value"]["expectedRowVersion"];
  /** Called with the outcome of every submission. */
  readonly onSubmitted?: (outcome: Outcome<InventoryAdjustResult>) => void;
}

/** What an operator calls this screen. The page decides where it goes. */
export const InventoryAdjustFormLabel = "adjust";

/**
 * The form for `wamn-wms:inventory/adjust@1.0.0`.
 *
 * It renders what the operator fills and nothing else. The reserved inputs
 * come from the runtime at submit time, and the operator never sees them.
 */
export function InventoryAdjustForm(props: InventoryAdjustFormProps) {
  const [refusal, setRefusal] = createSignal<{ text: string; member: string | null } | null>(null);
  const [done, setDone] = createSignal(false);

  const form = createForm(() => ({
    defaultValues: { ...props.initial } as Partial<InventoryAdjustRequest>,
    onSubmit: async ({ value }: { value: Partial<InventoryAdjustRequest> }) => {
      setDone(false);
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
      item = writeMember(item, ["value", "expectedRowVersion"], props.valueExpectedRowVersion);
      const outcome = await adjust(props.transport, [item]);
      props.onSubmitted?.(outcome);
      announceOutcome(outcome, InventoryAdjustFormLabel);
      setRefusal(
        outcome.status === "refused"
          ? { text: refusalSentence(outcome.code), member: refusedMember(outcome.detail) }
          : null,
      );
      setDone(outcome.status === "completed");
    },
  }));
  const [valuePalletIdOptions, setValuePalletIdOptions] = createSignal<PageState<PalletQueryRow>>(emptyPage<PalletQueryRow>());
  const [valuePalletIdSearch, setValuePalletIdSearch] = createSignal("");
  const readValuePalletIdOptions = async (cursor: string | null) => {
    let request = { requestId: newRequestId() } as PalletQueryRequest;
    if (valuePalletIdSearch() !== "") {
      request = writeMember(request, ["filter", "palletCode"], [valuePalletIdSearch()]) as PalletQueryRequest;
    }
    if (cursor !== null) {
      request = writeMember(request, ["cursor"], cursor) as PalletQueryRequest;
    }
    const outcome = await palletQuery(props.transport, [request]);
    if (outcome.status !== "completed") {
      return;
    }
    const rows = outcome.value.item as PalletQueryRow[];
    setValuePalletIdOptions(
      cursor === null
        ? firstPage(rows, outcome.value.nextCursor)
        : appendPage(valuePalletIdOptions(), rows, outcome.value.nextCursor),
    );
  };
  const readValuePalletIdRecord = async (key: string): Promise<PalletQueryRow | null> => {
    const request = writeMember({ requestId: newRequestId() }, ["id"], key) as PalletGetRequest;
    const outcome = await palletGet(props.transport, [request]);
    return outcome.status === "completed" ? ((outcome.value ?? null) as unknown as PalletQueryRow | null) : null;
  };
  void readValuePalletIdOptions(null);
  const [valueProductIdOptions, setValueProductIdOptions] = createSignal<PageState<ProductQueryRow>>(emptyPage<ProductQueryRow>());
  const [valueProductIdSearch, setValueProductIdSearch] = createSignal("");
  const readValueProductIdOptions = async (cursor: string | null) => {
    let request = { requestId: newRequestId() } as ProductQueryRequest;
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
    const request = writeMember({ requestId: newRequestId() }, ["id"], key) as ProductGetRequest;
    const outcome = await productGet(props.transport, [request]);
    return outcome.status === "completed" ? ((outcome.value ?? null) as unknown as ProductQueryRow | null) : null;
  };
  void readValueProductIdOptions(null);

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
        <form.Field name={`value.palletId`}>
          {(field) => (
            <RecordSelect
              label="pallet id"
              options={valuePalletIdOptions().rows}
              optionValue={(row) => String(row.id)}
              optionLabel={(row) => String(row.palletCode)}
              value={field().state.value == null ? null : String(field().state.value)}
              onChange={(value) => field().handleChange(value ?? "")}
              onSearch={(text) => {
                setValuePalletIdSearch(text);
                void readValuePalletIdOptions(null);
              }}
              hasNextPage={hasNextPage(valuePalletIdOptions())}
              onNextPage={() => void readValuePalletIdOptions(valuePalletIdOptions().cursor)}
              readRow={readValuePalletIdRecord}
              error={refusalMarks(refusal()?.member ?? null, "value.pallet_id") ? (refusal()?.text ?? null) : null}
            />
          )}
        </form.Field>
        <form.Field name={`value.productId`}>
          {(field) => (
            <RecordSelect
              label="product id"
              options={valueProductIdOptions().rows}
              optionValue={(row) => String(row.id)}
              optionLabel={(row) => String(row.productCode)}
              value={field().state.value == null ? null : String(field().state.value)}
              onChange={(value) => field().handleChange(value ?? "")}
              onSearch={(text) => {
                setValueProductIdSearch(text);
                void readValueProductIdOptions(null);
              }}
              hasNextPage={hasNextPage(valueProductIdOptions())}
              onNextPage={() => void readValueProductIdOptions(valueProductIdOptions().cursor)}
              readRow={readValueProductIdRecord}
              error={refusalMarks(refusal()?.member ?? null, "value.product_id") ? (refusal()?.text ?? null) : null}
            />
          )}
        </form.Field>
        <form.Field name={`value.quantity`}>
          {(field) => (
            <TextField
              label="quantity"
              type="text"
              value={String(field().state.value ?? "")}
              onInput={(value) => field().handleChange(value)}
              error={refusalMarks(refusal()?.member ?? null, "value.quantity") ? (refusal()?.text ?? null) : null}
            />
          )}
        </form.Field>
        <form.Field name={`value.reasonCode`}>
          {(field) => (
            <TextField
              label="reason code"
              type="text"
              value={String(field().state.value ?? "")}
              onInput={(value) => field().handleChange(value)}
              error={refusalMarks(refusal()?.member ?? null, "value.reason_code") ? (refusal()?.text ?? null) : null}
            />
          )}
        </form.Field>
        <form.Field name={`value.status`}>
          {(field) => (
            <ChoiceField
              label="status"
              allowEmpty={false}
              choices={[
                { value: "available", text: "available" },
                { value: "held", text: "held" },
              ]}
              value={String(field().state.value ?? "")}
              onChange={(value) => field().handleChange(value as "available" | "held")}
              error={refusalMarks(refusal()?.member ?? null, "value.status") ? (refusal()?.text ?? null) : null}
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

/** Columns of `wamn-wms:inventory/aggregate@1.0.0`, in contract order. */
const AGGREGATE_COLUMNS: ColumnDef<GridFeatures, InventoryAggregateRow>[] = [
  {
    accessorKey: "locationId",
    header: "location id",
    cell: (cell) => cellText(cell.getValue() as JsonValue, "uuid"),
  },
  {
    accessorKey: "palletCount",
    header: "pallet count",
    cell: (cell) => cellText(cell.getValue() as JsonValue, "int32"),
  },
  {
    accessorKey: "productId",
    header: "product id",
    cell: (cell) => cellText(cell.getValue() as JsonValue, "uuid"),
  },
  {
    accessorKey: "quantity",
    header: "quantity",
    cell: (cell) => cellText(cell.getValue() as JsonValue, "numeric"),
  },
  {
    accessorKey: "status",
    header: "status",
    cell: (cell) => (
      <Show when={cellText(cell.getValue() as JsonValue, "text") !== ""}>
        <Badge variant="outline">{cellText(cell.getValue() as JsonValue, "text")}</Badge>
      </Show>
    ),
  },
];

/** What the table for `wamn-wms:inventory/aggregate@1.0.0` takes. */
export interface InventoryAggregateTableProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** Input the parent fixes, which the operator does not edit. */
  readonly fixed?: Partial<InventoryAggregateRequest>;
  /** Called when the operator picks one row. */
  readonly onRowSelect?: (row: InventoryAggregateRow) => void;
  /** Called with every outcome this screen reads. */
  readonly onOutcome?: (outcome: Outcome<InventoryAggregateResult>) => void;
}

/** What an operator calls this screen. The page decides where it goes. */
export const InventoryAggregateTableLabel = "aggregate";

/**
 * The table for `wamn-wms:inventory/aggregate@1.0.0`.
 *
 * It owns its page controls and its rows. A change to a control clears the
 * rows, because a cursor names a position in the list the old input produced.
 *
 * It reads when the operator asks, and not when it mounts, because a read is
 * a request that the operator did not send yet.
 */
export function InventoryAggregateTable(props: InventoryAggregateTableProps) {
  const controls = (): Partial<InventoryAggregateRequest> => ({});
  const [page, setPage] = createSignal<PageState<InventoryAggregateRow>>(emptyPage<InventoryAggregateRow>());

  const read = async (cursor: string | null) => {
    setPage(startRead(page()));
    const request = {
      ...controls(),
      ...props.fixed,
      requestId: newRequestId(),
    } as InventoryAggregateRequest;
    const sent = request;
    const outcome = await aggregate(props.transport, [sent]);
    props.onOutcome?.(outcome);
    if (outcome.status !== "completed") {
      announceOutcome(outcome, InventoryAggregateTableLabel);
      setPage(failedRead(page(), outcome));
      return;
    }
    const rows = outcome.value.rows;
    setPage(cursor === null ? firstPage(rows, null) : appendPage(page(), rows, null));
  };

  const restart = () => {
    setPage(emptyPage<InventoryAggregateRow>());
    void read(null);
  };

  const table = createTable({
    features: gridFeatures,
    get data() {
      return page().rows as InventoryAggregateRow[];
    },
    columns: AGGREGATE_COLUMNS,
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
        emptyMessage={page().refusal}
        onRowClick={(row) => props.onRowSelect?.(row)}
      >
        <DataGridContainer>
          <DataGridTable />
        </DataGridContainer>
      </DataGrid>
    </TableScreen>
  );
}

/** What an operator types for `wamn-wms:inventory/merge@1.0.0`. */
const MERGE_INPUT = z.object({
  value: z
    .object({
      sourcePalletId: z.string().regex(UUID_TEXT, "expected a UUID"),
      targetPalletId: z.string().regex(UUID_TEXT, "expected a UUID"),
    })
    .optional(),
});

/** What the form for `wamn-wms:inventory/merge@1.0.0` can start with. */
export interface InventoryMergeFormInitial {
  value?: {
    sourcePalletId?: Uuid;
    targetPalletId?: Uuid;
  };
}

/** What the form for `wamn-wms:inventory/merge@1.0.0` takes. */
export interface InventoryMergeFormProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** Values the form starts with. */
  readonly initial?: InventoryMergeFormInitial;
  /** Called with the outcome of every submission. */
  readonly onSubmitted?: (outcome: Outcome<InventoryMergeResult>) => void;
}

/** What an operator calls this screen. The page decides where it goes. */
export const InventoryMergeFormLabel = "merge";

/**
 * The form for `wamn-wms:inventory/merge@1.0.0`.
 *
 * It renders what the operator fills and nothing else. The reserved inputs
 * come from the runtime at submit time, and the operator never sees them.
 */
export function InventoryMergeForm(props: InventoryMergeFormProps) {
  const [refusal, setRefusal] = createSignal<{ text: string; member: string | null } | null>(null);
  const [done, setDone] = createSignal(false);

  const form = createForm(() => ({
    defaultValues: { ...props.initial } as Partial<InventoryMergeRequest>,
    onSubmit: async ({ value }: { value: Partial<InventoryMergeRequest> }) => {
      setDone(false);
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
      const valueTargetPalletIdChosen = valueTargetPalletIdRevision();
      if (valueTargetPalletIdChosen === null) {
        setRefusal({ text: "Choose the record from its list.", member: "value.target_pallet_id" });
        return;
      }
      item = writeMember(item, ["value", "expectedRowVersion"], valueTargetPalletIdChosen);
      const outcome = await merge(props.transport, [item]);
      props.onSubmitted?.(outcome);
      announceOutcome(outcome, InventoryMergeFormLabel);
      setRefusal(
        outcome.status === "refused"
          ? { text: refusalSentence(outcome.code), member: refusedMember(outcome.detail) }
          : null,
      );
      setDone(outcome.status === "completed");
    },
  }));
  const [valueSourcePalletIdOptions, setValueSourcePalletIdOptions] = createSignal<PageState<PalletQueryRow>>(emptyPage<PalletQueryRow>());
  const [valueSourcePalletIdSearch, setValueSourcePalletIdSearch] = createSignal("");
  const readValueSourcePalletIdOptions = async (cursor: string | null) => {
    let request = { requestId: newRequestId() } as PalletQueryRequest;
    if (valueSourcePalletIdSearch() !== "") {
      request = writeMember(request, ["filter", "palletCode"], [valueSourcePalletIdSearch()]) as PalletQueryRequest;
    }
    if (cursor !== null) {
      request = writeMember(request, ["cursor"], cursor) as PalletQueryRequest;
    }
    const outcome = await palletQuery(props.transport, [request]);
    if (outcome.status !== "completed") {
      return;
    }
    const rows = outcome.value.item as PalletQueryRow[];
    setValueSourcePalletIdOptions(
      cursor === null
        ? firstPage(rows, outcome.value.nextCursor)
        : appendPage(valueSourcePalletIdOptions(), rows, outcome.value.nextCursor),
    );
  };
  const readValueSourcePalletIdRecord = async (key: string): Promise<PalletQueryRow | null> => {
    const request = writeMember({ requestId: newRequestId() }, ["id"], key) as PalletGetRequest;
    const outcome = await palletGet(props.transport, [request]);
    return outcome.status === "completed" ? ((outcome.value ?? null) as unknown as PalletQueryRow | null) : null;
  };
  void readValueSourcePalletIdOptions(null);
  const [valueTargetPalletIdOptions, setValueTargetPalletIdOptions] = createSignal<PageState<PalletQueryRow>>(emptyPage<PalletQueryRow>());
  const [valueTargetPalletIdSearch, setValueTargetPalletIdSearch] = createSignal("");
  const [valueTargetPalletIdRevision, setValueTargetPalletIdRevision] = createSignal<PalletQueryRow["rowVersion"] | null>(null);
  const readValueTargetPalletIdOptions = async (cursor: string | null) => {
    let request = { requestId: newRequestId() } as PalletQueryRequest;
    if (valueTargetPalletIdSearch() !== "") {
      request = writeMember(request, ["filter", "palletCode"], [valueTargetPalletIdSearch()]) as PalletQueryRequest;
    }
    if (cursor !== null) {
      request = writeMember(request, ["cursor"], cursor) as PalletQueryRequest;
    }
    const outcome = await palletQuery(props.transport, [request]);
    if (outcome.status !== "completed") {
      return;
    }
    const rows = outcome.value.item as PalletQueryRow[];
    setValueTargetPalletIdOptions(
      cursor === null
        ? firstPage(rows, outcome.value.nextCursor)
        : appendPage(valueTargetPalletIdOptions(), rows, outcome.value.nextCursor),
    );
  };
  const readValueTargetPalletIdRecord = async (key: string): Promise<PalletQueryRow | null> => {
    const request = writeMember({ requestId: newRequestId() }, ["id"], key) as PalletGetRequest;
    const outcome = await palletGet(props.transport, [request]);
    return outcome.status === "completed" ? ((outcome.value ?? null) as unknown as PalletQueryRow | null) : null;
  };
  void readValueTargetPalletIdOptions(null);

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
        <form.Field name={`value.sourcePalletId`}>
          {(field) => (
            <RecordSelect
              label="source pallet id"
              options={valueSourcePalletIdOptions().rows}
              optionValue={(row) => String(row.id)}
              optionLabel={(row) => String(row.palletCode)}
              value={field().state.value == null ? null : String(field().state.value)}
              onChange={(value) => field().handleChange(value ?? "")}
              onSearch={(text) => {
                setValueSourcePalletIdSearch(text);
                void readValueSourcePalletIdOptions(null);
              }}
              hasNextPage={hasNextPage(valueSourcePalletIdOptions())}
              onNextPage={() => void readValueSourcePalletIdOptions(valueSourcePalletIdOptions().cursor)}
              readRow={readValueSourcePalletIdRecord}
              error={refusalMarks(refusal()?.member ?? null, "value.source_pallet_id") ? (refusal()?.text ?? null) : null}
            />
          )}
        </form.Field>
        <form.Field name={`value.targetPalletId`}>
          {(field) => (
            <RecordSelect
              label="target pallet id"
              options={valueTargetPalletIdOptions().rows}
              optionValue={(row) => String(row.id)}
              optionLabel={(row) => String(row.palletCode)}
              value={field().state.value == null ? null : String(field().state.value)}
              onChange={(value) => field().handleChange(value ?? "")}
              onRow={(row) => setValueTargetPalletIdRevision(row?.rowVersion ?? null)}
              onSearch={(text) => {
                setValueTargetPalletIdSearch(text);
                void readValueTargetPalletIdOptions(null);
              }}
              hasNextPage={hasNextPage(valueTargetPalletIdOptions())}
              onNextPage={() => void readValueTargetPalletIdOptions(valueTargetPalletIdOptions().cursor)}
              readRow={readValueTargetPalletIdRecord}
              error={refusalMarks(refusal()?.member ?? null, "value.target_pallet_id") ? (refusal()?.text ?? null) : null}
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

/** What an operator types for `wamn-wms:inventory/move@1.0.0`. */
const MOVE_INPUT = z.object({
  value: z
    .object({
      palletId: z.string().regex(UUID_TEXT, "expected a UUID"),
      toLocationId: z.string().regex(UUID_TEXT, "expected a UUID"),
    })
    .optional(),
});

/** What the form for `wamn-wms:inventory/move@1.0.0` can start with. */
export interface InventoryMoveFormInitial {
  value?: {
    palletId?: Uuid;
    toLocationId?: Uuid;
  };
}

/** What the form for `wamn-wms:inventory/move@1.0.0` takes. */
export interface InventoryMoveFormProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** Values the form starts with. */
  readonly initial?: InventoryMoveFormInitial;
  /** The revision this command sends. The release binds no read that supplies it. */
  readonly valueExpectedRowVersion: InventoryMoveRequest["value"]["expectedRowVersion"];
  /** Called with the outcome of every submission. */
  readonly onSubmitted?: (outcome: Outcome<InventoryMoveResult>) => void;
}

/** What an operator calls this screen. The page decides where it goes. */
export const InventoryMoveFormLabel = "move";

/**
 * The form for `wamn-wms:inventory/move@1.0.0`.
 *
 * It renders what the operator fills and nothing else. The reserved inputs
 * come from the runtime at submit time, and the operator never sees them.
 */
export function InventoryMoveForm(props: InventoryMoveFormProps) {
  const [refusal, setRefusal] = createSignal<{ text: string; member: string | null } | null>(null);
  const [done, setDone] = createSignal(false);

  const form = createForm(() => ({
    defaultValues: { ...props.initial } as Partial<InventoryMoveRequest>,
    onSubmit: async ({ value }: { value: Partial<InventoryMoveRequest> }) => {
      setDone(false);
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
      item = writeMember(item, ["value", "expectedRowVersion"], props.valueExpectedRowVersion);
      const outcome = await move(props.transport, [item]);
      props.onSubmitted?.(outcome);
      announceOutcome(outcome, InventoryMoveFormLabel);
      setRefusal(
        outcome.status === "refused"
          ? { text: refusalSentence(outcome.code), member: refusedMember(outcome.detail) }
          : null,
      );
      setDone(outcome.status === "completed");
    },
  }));
  const [valuePalletIdOptions, setValuePalletIdOptions] = createSignal<PageState<PalletQueryRow>>(emptyPage<PalletQueryRow>());
  const [valuePalletIdSearch, setValuePalletIdSearch] = createSignal("");
  const readValuePalletIdOptions = async (cursor: string | null) => {
    let request = { requestId: newRequestId() } as PalletQueryRequest;
    if (valuePalletIdSearch() !== "") {
      request = writeMember(request, ["filter", "palletCode"], [valuePalletIdSearch()]) as PalletQueryRequest;
    }
    if (cursor !== null) {
      request = writeMember(request, ["cursor"], cursor) as PalletQueryRequest;
    }
    const outcome = await palletQuery(props.transport, [request]);
    if (outcome.status !== "completed") {
      return;
    }
    const rows = outcome.value.item as PalletQueryRow[];
    setValuePalletIdOptions(
      cursor === null
        ? firstPage(rows, outcome.value.nextCursor)
        : appendPage(valuePalletIdOptions(), rows, outcome.value.nextCursor),
    );
  };
  const readValuePalletIdRecord = async (key: string): Promise<PalletQueryRow | null> => {
    const request = writeMember({ requestId: newRequestId() }, ["id"], key) as PalletGetRequest;
    const outcome = await palletGet(props.transport, [request]);
    return outcome.status === "completed" ? ((outcome.value ?? null) as unknown as PalletQueryRow | null) : null;
  };
  void readValuePalletIdOptions(null);
  const [valueToLocationIdOptions, setValueToLocationIdOptions] = createSignal<PageState<LocationQueryRow>>(emptyPage<LocationQueryRow>());
  const [valueToLocationIdSearch, setValueToLocationIdSearch] = createSignal("");
  const readValueToLocationIdOptions = async (cursor: string | null) => {
    let request = { requestId: newRequestId() } as LocationQueryRequest;
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
    const request = writeMember({ requestId: newRequestId() }, ["id"], key) as LocationGetRequest;
    const outcome = await locationGet(props.transport, [request]);
    return outcome.status === "completed" ? ((outcome.value ?? null) as unknown as LocationQueryRow | null) : null;
  };
  void readValueToLocationIdOptions(null);

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
        <form.Field name={`value.palletId`}>
          {(field) => (
            <RecordSelect
              label="pallet id"
              options={valuePalletIdOptions().rows}
              optionValue={(row) => String(row.id)}
              optionLabel={(row) => String(row.palletCode)}
              value={field().state.value == null ? null : String(field().state.value)}
              onChange={(value) => field().handleChange(value ?? "")}
              onSearch={(text) => {
                setValuePalletIdSearch(text);
                void readValuePalletIdOptions(null);
              }}
              hasNextPage={hasNextPage(valuePalletIdOptions())}
              onNextPage={() => void readValuePalletIdOptions(valuePalletIdOptions().cursor)}
              readRow={readValuePalletIdRecord}
              error={refusalMarks(refusal()?.member ?? null, "value.pallet_id") ? (refusal()?.text ?? null) : null}
            />
          )}
        </form.Field>
        <form.Field name={`value.toLocationId`}>
          {(field) => (
            <RecordSelect
              label="to location id"
              options={valueToLocationIdOptions().rows}
              optionValue={(row) => String(row.id)}
              optionLabel={(row) => String(row.locationCode)}
              value={field().state.value == null ? null : String(field().state.value)}
              onChange={(value) => field().handleChange(value ?? "")}
              onSearch={(text) => {
                setValueToLocationIdSearch(text);
                void readValueToLocationIdOptions(null);
              }}
              hasNextPage={hasNextPage(valueToLocationIdOptions())}
              onNextPage={() => void readValueToLocationIdOptions(valueToLocationIdOptions().cursor)}
              readRow={readValueToLocationIdRecord}
              error={refusalMarks(refusal()?.member ?? null, "value.to_location_id") ? (refusal()?.text ?? null) : null}
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

/** What an operator types for `wamn-wms:inventory/split@1.0.0`. */
const SPLIT_INPUT = z.object({
  value: z
    .object({
      newPalletCode: z.string(),
      productId: z.string().regex(UUID_TEXT, "expected a UUID"),
      quantity: z.string().regex(NUMERIC_TEXT, "expected decimal text"),
      sourcePalletId: z.string().regex(UUID_TEXT, "expected a UUID"),
      status: z.enum(["available", "held"]),
      toLocationId: z.string().regex(UUID_TEXT, "expected a UUID"),
    })
    .optional(),
});

/** What the form for `wamn-wms:inventory/split@1.0.0` can start with. */
export interface InventorySplitFormInitial {
  value?: {
    newPalletCode?: string;
    productId?: Uuid;
    quantity?: Numeric;
    sourcePalletId?: Uuid;
    status?: "available" | "held";
    toLocationId?: Uuid;
  };
}

/** What the form for `wamn-wms:inventory/split@1.0.0` takes. */
export interface InventorySplitFormProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** Values the form starts with. */
  readonly initial?: InventorySplitFormInitial;
  /** The revision this command sends. The release binds no read that supplies it. */
  readonly valueExpectedRowVersion: InventorySplitRequest["value"]["expectedRowVersion"];
  /** Called with the outcome of every submission. */
  readonly onSubmitted?: (outcome: Outcome<InventorySplitResult>) => void;
}

/** What an operator calls this screen. The page decides where it goes. */
export const InventorySplitFormLabel = "split";

/**
 * The form for `wamn-wms:inventory/split@1.0.0`.
 *
 * It renders what the operator fills and nothing else. The reserved inputs
 * come from the runtime at submit time, and the operator never sees them.
 */
export function InventorySplitForm(props: InventorySplitFormProps) {
  const [refusal, setRefusal] = createSignal<{ text: string; member: string | null } | null>(null);
  const [done, setDone] = createSignal(false);

  const form = createForm(() => ({
    defaultValues: { ...props.initial } as Partial<InventorySplitRequest>,
    onSubmit: async ({ value }: { value: Partial<InventorySplitRequest> }) => {
      setDone(false);
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
      item = writeMember(item, ["value", "expectedRowVersion"], props.valueExpectedRowVersion);
      const outcome = await split(props.transport, [item]);
      props.onSubmitted?.(outcome);
      announceOutcome(outcome, InventorySplitFormLabel);
      setRefusal(
        outcome.status === "refused"
          ? { text: refusalSentence(outcome.code), member: refusedMember(outcome.detail) }
          : null,
      );
      setDone(outcome.status === "completed");
    },
  }));
  const [valueProductIdOptions, setValueProductIdOptions] = createSignal<PageState<ProductQueryRow>>(emptyPage<ProductQueryRow>());
  const [valueProductIdSearch, setValueProductIdSearch] = createSignal("");
  const readValueProductIdOptions = async (cursor: string | null) => {
    let request = { requestId: newRequestId() } as ProductQueryRequest;
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
    const request = writeMember({ requestId: newRequestId() }, ["id"], key) as ProductGetRequest;
    const outcome = await productGet(props.transport, [request]);
    return outcome.status === "completed" ? ((outcome.value ?? null) as unknown as ProductQueryRow | null) : null;
  };
  void readValueProductIdOptions(null);
  const [valueSourcePalletIdOptions, setValueSourcePalletIdOptions] = createSignal<PageState<PalletQueryRow>>(emptyPage<PalletQueryRow>());
  const [valueSourcePalletIdSearch, setValueSourcePalletIdSearch] = createSignal("");
  const readValueSourcePalletIdOptions = async (cursor: string | null) => {
    let request = { requestId: newRequestId() } as PalletQueryRequest;
    if (valueSourcePalletIdSearch() !== "") {
      request = writeMember(request, ["filter", "palletCode"], [valueSourcePalletIdSearch()]) as PalletQueryRequest;
    }
    if (cursor !== null) {
      request = writeMember(request, ["cursor"], cursor) as PalletQueryRequest;
    }
    const outcome = await palletQuery(props.transport, [request]);
    if (outcome.status !== "completed") {
      return;
    }
    const rows = outcome.value.item as PalletQueryRow[];
    setValueSourcePalletIdOptions(
      cursor === null
        ? firstPage(rows, outcome.value.nextCursor)
        : appendPage(valueSourcePalletIdOptions(), rows, outcome.value.nextCursor),
    );
  };
  const readValueSourcePalletIdRecord = async (key: string): Promise<PalletQueryRow | null> => {
    const request = writeMember({ requestId: newRequestId() }, ["id"], key) as PalletGetRequest;
    const outcome = await palletGet(props.transport, [request]);
    return outcome.status === "completed" ? ((outcome.value ?? null) as unknown as PalletQueryRow | null) : null;
  };
  void readValueSourcePalletIdOptions(null);
  const [valueToLocationIdOptions, setValueToLocationIdOptions] = createSignal<PageState<LocationQueryRow>>(emptyPage<LocationQueryRow>());
  const [valueToLocationIdSearch, setValueToLocationIdSearch] = createSignal("");
  const readValueToLocationIdOptions = async (cursor: string | null) => {
    let request = { requestId: newRequestId() } as LocationQueryRequest;
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
    const request = writeMember({ requestId: newRequestId() }, ["id"], key) as LocationGetRequest;
    const outcome = await locationGet(props.transport, [request]);
    return outcome.status === "completed" ? ((outcome.value ?? null) as unknown as LocationQueryRow | null) : null;
  };
  void readValueToLocationIdOptions(null);

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
        <form.Field name={`value.newPalletCode`}>
          {(field) => (
            <TextField
              label="new pallet code"
              type="text"
              value={String(field().state.value ?? "")}
              onInput={(value) => field().handleChange(value)}
              error={refusalMarks(refusal()?.member ?? null, "value.new_pallet_code") ? (refusal()?.text ?? null) : null}
            />
          )}
        </form.Field>
        <form.Field name={`value.productId`}>
          {(field) => (
            <RecordSelect
              label="product id"
              options={valueProductIdOptions().rows}
              optionValue={(row) => String(row.id)}
              optionLabel={(row) => String(row.productCode)}
              value={field().state.value == null ? null : String(field().state.value)}
              onChange={(value) => field().handleChange(value ?? "")}
              onSearch={(text) => {
                setValueProductIdSearch(text);
                void readValueProductIdOptions(null);
              }}
              hasNextPage={hasNextPage(valueProductIdOptions())}
              onNextPage={() => void readValueProductIdOptions(valueProductIdOptions().cursor)}
              readRow={readValueProductIdRecord}
              error={refusalMarks(refusal()?.member ?? null, "value.product_id") ? (refusal()?.text ?? null) : null}
            />
          )}
        </form.Field>
        <form.Field name={`value.quantity`}>
          {(field) => (
            <TextField
              label="quantity"
              type="text"
              value={String(field().state.value ?? "")}
              onInput={(value) => field().handleChange(value)}
              error={refusalMarks(refusal()?.member ?? null, "value.quantity") ? (refusal()?.text ?? null) : null}
            />
          )}
        </form.Field>
        <form.Field name={`value.sourcePalletId`}>
          {(field) => (
            <RecordSelect
              label="source pallet id"
              options={valueSourcePalletIdOptions().rows}
              optionValue={(row) => String(row.id)}
              optionLabel={(row) => String(row.palletCode)}
              value={field().state.value == null ? null : String(field().state.value)}
              onChange={(value) => field().handleChange(value ?? "")}
              onSearch={(text) => {
                setValueSourcePalletIdSearch(text);
                void readValueSourcePalletIdOptions(null);
              }}
              hasNextPage={hasNextPage(valueSourcePalletIdOptions())}
              onNextPage={() => void readValueSourcePalletIdOptions(valueSourcePalletIdOptions().cursor)}
              readRow={readValueSourcePalletIdRecord}
              error={refusalMarks(refusal()?.member ?? null, "value.source_pallet_id") ? (refusal()?.text ?? null) : null}
            />
          )}
        </form.Field>
        <form.Field name={`value.status`}>
          {(field) => (
            <ChoiceField
              label="status"
              allowEmpty={false}
              choices={[
                { value: "available", text: "available" },
                { value: "held", text: "held" },
              ]}
              value={String(field().state.value ?? "")}
              onChange={(value) => field().handleChange(value as "available" | "held")}
              error={refusalMarks(refusal()?.member ?? null, "value.status") ? (refusal()?.text ?? null) : null}
            />
          )}
        </form.Field>
        <form.Field name={`value.toLocationId`}>
          {(field) => (
            <RecordSelect
              label="to location id"
              options={valueToLocationIdOptions().rows}
              optionValue={(row) => String(row.id)}
              optionLabel={(row) => String(row.locationCode)}
              value={field().state.value == null ? null : String(field().state.value)}
              onChange={(value) => field().handleChange(value ?? "")}
              onSearch={(text) => {
                setValueToLocationIdSearch(text);
                void readValueToLocationIdOptions(null);
              }}
              hasNextPage={hasNextPage(valueToLocationIdOptions())}
              onNextPage={() => void readValueToLocationIdOptions(valueToLocationIdOptions().cursor)}
              readRow={readValueToLocationIdRecord}
              error={refusalMarks(refusal()?.member ?? null, "value.to_location_id") ? (refusal()?.text ?? null) : null}
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
