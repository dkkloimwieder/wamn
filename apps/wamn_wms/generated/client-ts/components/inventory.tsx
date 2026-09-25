// @generated from the client-contract IR; do not edit.
//
// `inventory` components. Each one calls the bindings and the runtime, and
// nothing else.

import { Show, createResource, createSignal } from "solid-js";
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
  readMember,
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
  writeControl,
  writeMember,
} from "@wamn/web-runtime";
import {
  Badge,
  Button,
  DataGrid,
  DataGridContainer,
  DataGridTable,
  DetailItem,
  DetailList,
  FieldError,
  FieldGroup,
  FormActions,
  FormDone,
  RecordSelect,
  TableScreen,
  TextField,
  announceOutcome,
  createRecordLabels,
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
  get,
  get as inventoryGet,
  merge,
  move,
  query,
  query as inventoryQuery,
  split,
  type InventoryAdjustRequest,
  type InventoryAdjustResult,
  type InventoryAggregateRequest,
  type InventoryAggregateResult,
  type InventoryAggregateRow,
  type InventoryGetRequest,
  type InventoryGetResult,
  type InventoryMergeRequest,
  type InventoryMergeResult,
  type InventoryMoveRequest,
  type InventoryMoveResult,
  type InventoryQueryRequest,
  type InventoryQueryResult,
  type InventoryQueryRow,
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
  get as packagingGet,
  query as packagingQuery,
  type PackagingGetRequest,
  type PackagingQueryRequest,
  type PackagingQueryRow,
} from "../packaging.js";
import {
  get as productGet,
  type ProductGetRequest,
} from "../product.js";

/** What the release accepts: one UUID, hyphenated. */
const UUID_TEXT = /^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$/;

/** What the release accepts: decimal text without an exponent. */
const NUMERIC_TEXT = /^[+-]?(\d+(\.\d*)?|\.\d+)$/;

/** What an operator types for `wamn-wms:inventory/adjust@1.0.0`. */
const ADJUST_INPUT = z.object({
  value: z
    .object({
      inventoryId: z.string().regex(UUID_TEXT, "expected a UUID"),
      reason: z.string(),
      toQuantity: z.string().regex(NUMERIC_TEXT, "expected decimal text"),
    })
    .optional(),
});

/** What the form for `wamn-wms:inventory/adjust@1.0.0` can start with. */
export interface InventoryAdjustFormInitial {
  value?: {
    inventoryId?: Uuid;
    reason?: string;
    toQuantity?: Numeric;
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
  const [valueInventoryIdOptions, setValueInventoryIdOptions] = createSignal<PageState<InventoryQueryRow>>(emptyPage<InventoryQueryRow>());
  const readValueInventoryIdOptions = async (cursor: string | null) => {
    let request = {} as InventoryQueryRequest;
    if (cursor !== null) {
      request = writeMember(request, ["cursor"], cursor) as InventoryQueryRequest;
    }
    const outcome = await inventoryQuery(props.transport, [request]);
    if (outcome.status !== "completed") {
      return;
    }
    const rows = outcome.value.item as InventoryQueryRow[];
    setValueInventoryIdOptions(
      cursor === null
        ? firstPage(rows, outcome.value.nextCursor)
        : appendPage(valueInventoryIdOptions(), rows, outcome.value.nextCursor),
    );
  };
  const readValueInventoryIdRecord = async (key: string): Promise<InventoryQueryRow | null> => {
    const request = writeMember({}, ["id"], key) as InventoryGetRequest;
    const outcome = await inventoryGet(props.transport, [request]);
    return outcome.status === "completed" ? ((outcome.value ?? null) as unknown as InventoryQueryRow | null) : null;
  };
  void readValueInventoryIdOptions(null);

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
        <form.Field name={`value.inventoryId`}>
          {(field) => (
            <RecordSelect
              label="inventory id"
              options={valueInventoryIdOptions().rows}
              optionValue={(row) => String(row.id)}
              optionLabel={(row) => String(row.disposition)}
              value={field().state.value == null ? null : String(field().state.value)}
              onChange={(value) => field().handleChange(value ?? "")}
              hasNextPage={hasNextPage(valueInventoryIdOptions())}
              onNextPage={() => void readValueInventoryIdOptions(valueInventoryIdOptions().cursor)}
              readRow={readValueInventoryIdRecord}
              error={refusalMarks(refusal()?.member ?? null, "value.inventory_id") ? (refusal()?.text ?? null) : null}
            />
          )}
        </form.Field>
        <form.Field name={`value.reason`}>
          {(field) => (
            <TextField
              label="reason"
              type="text"
              value={String(field().state.value ?? "")}
              onInput={(value) => field().handleChange(value)}
              error={refusalMarks(refusal()?.member ?? null, "value.reason") ? (refusal()?.text ?? null) : null}
            />
          )}
        </form.Field>
        <form.Field name={`value.toQuantity`}>
          {(field) => (
            <TextField
              label="to quantity"
              type="text"
              value={String(field().state.value ?? "")}
              onInput={(value) => field().handleChange(value)}
              error={refusalMarks(refusal()?.member ?? null, "value.to_quantity") ? (refusal()?.text ?? null) : null}
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
    accessorKey: "disposition",
    header: "disposition",
    cell: (cell) => (
      <Show when={cellText(cell.getValue() as JsonValue, "text") !== ""}>
        <Badge variant="outline">{cellText(cell.getValue() as JsonValue, "text")}</Badge>
      </Show>
    ),
  },
  {
    accessorKey: "locationId",
    header: "location id",
    cell: (cell) => cellText(cell.getValue() as JsonValue, "uuid"),
  },
  {
    accessorKey: "packagingCount",
    header: "packaging count",
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

/** The record that the detail for `wamn-wms:inventory/get@1.0.0` reads. */
export interface InventoryGetDetailInput {
  readonly id: Uuid;
}

/** What the detail screen for `wamn-wms:inventory/get@1.0.0` takes. */
export interface InventoryGetDetailProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** The input that names the record. */
  readonly input: InventoryGetDetailInput;
  /** Called with every outcome this screen reads. */
  readonly onOutcome?: (outcome: Outcome<InventoryGetResult>) => void;
}

/** What an operator calls this screen. The page decides where it goes. */
export const InventoryGetDetailLabel = "get";

/**
 * The detail screen for `wamn-wms:inventory/get@1.0.0`.
 *
 * It reads when it mounts and again whenever its input changes, because the
 * input names the record it shows.
 */
export function InventoryGetDetail(props: InventoryGetDetailProps) {
  const [outcome] = createResource(
    () => props.input,
    async (input: InventoryGetDetailInput) => {
      const read = await get(props.transport, [
        { ...input } as InventoryGetRequest,
      ]);
      props.onOutcome?.(read);
      if (read.status !== "completed") {
        announceOutcome(read, InventoryGetDetailLabel);
      }
      return read;
    },
  );
  const record = (): InventoryGetResult | undefined => {
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
        <DetailItem term="created at">{cellText(readMember(record(), ["createdAt"]), "timestamptz")}</DetailItem>
        <DetailItem term="disposition">{cellText(readMember(record(), ["disposition"]), "text")}</DetailItem>
        <DetailItem term="id">{cellText(readMember(record(), ["id"]), "uuid")}</DetailItem>
        <DetailItem term="lifecycle">{cellText(readMember(record(), ["lifecycle"]), "text")}</DetailItem>
        <DetailItem term="location id">{cellText(readMember(record(), ["locationId"]), "uuid")}</DetailItem>
        <DetailItem term="packaging id">{cellText(readMember(record(), ["packagingId"]), "uuid")}</DetailItem>
        <DetailItem term="product id">{cellText(readMember(record(), ["productId"]), "uuid")}</DetailItem>
        <DetailItem term="quantity">{cellText(readMember(record(), ["quantity"]), "numeric")}</DetailItem>
        <DetailItem term="row version">{cellText(readMember(record(), ["rowVersion"]), "int32")}</DetailItem>
      </DetailList>
    </section>
  );
}

/** What an operator types for `wamn-wms:inventory/merge@1.0.0`. */
const MERGE_INPUT = z.object({
  value: z
    .object({
      fromInventoryId: z.string().regex(UUID_TEXT, "expected a UUID"),
      toInventoryId: z.string().regex(UUID_TEXT, "expected a UUID"),
    })
    .optional(),
});

/** What the form for `wamn-wms:inventory/merge@1.0.0` can start with. */
export interface InventoryMergeFormInitial {
  value?: {
    fromInventoryId?: Uuid;
    toInventoryId?: Uuid;
  };
}

/** What the form for `wamn-wms:inventory/merge@1.0.0` takes. */
export interface InventoryMergeFormProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** Values the form starts with. */
  readonly initial?: InventoryMergeFormInitial;
  /** The revision this command sends. The release binds no read that supplies it. */
  readonly valueExpectedFromRowVersion: InventoryMergeRequest["value"]["expectedFromRowVersion"];
  /** The revision this command sends. The release binds no read that supplies it. */
  readonly valueExpectedToRowVersion: InventoryMergeRequest["value"]["expectedToRowVersion"];
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
      item = writeMember(item, ["value", "expectedFromRowVersion"], props.valueExpectedFromRowVersion);
      item = writeMember(item, ["value", "expectedToRowVersion"], props.valueExpectedToRowVersion);
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
  const [valueFromInventoryIdOptions, setValueFromInventoryIdOptions] = createSignal<PageState<InventoryQueryRow>>(emptyPage<InventoryQueryRow>());
  const readValueFromInventoryIdOptions = async (cursor: string | null) => {
    let request = {} as InventoryQueryRequest;
    if (cursor !== null) {
      request = writeMember(request, ["cursor"], cursor) as InventoryQueryRequest;
    }
    const outcome = await inventoryQuery(props.transport, [request]);
    if (outcome.status !== "completed") {
      return;
    }
    const rows = outcome.value.item as InventoryQueryRow[];
    setValueFromInventoryIdOptions(
      cursor === null
        ? firstPage(rows, outcome.value.nextCursor)
        : appendPage(valueFromInventoryIdOptions(), rows, outcome.value.nextCursor),
    );
  };
  const readValueFromInventoryIdRecord = async (key: string): Promise<InventoryQueryRow | null> => {
    const request = writeMember({}, ["id"], key) as InventoryGetRequest;
    const outcome = await inventoryGet(props.transport, [request]);
    return outcome.status === "completed" ? ((outcome.value ?? null) as unknown as InventoryQueryRow | null) : null;
  };
  void readValueFromInventoryIdOptions(null);
  const [valueToInventoryIdOptions, setValueToInventoryIdOptions] = createSignal<PageState<InventoryQueryRow>>(emptyPage<InventoryQueryRow>());
  const readValueToInventoryIdOptions = async (cursor: string | null) => {
    let request = {} as InventoryQueryRequest;
    if (cursor !== null) {
      request = writeMember(request, ["cursor"], cursor) as InventoryQueryRequest;
    }
    const outcome = await inventoryQuery(props.transport, [request]);
    if (outcome.status !== "completed") {
      return;
    }
    const rows = outcome.value.item as InventoryQueryRow[];
    setValueToInventoryIdOptions(
      cursor === null
        ? firstPage(rows, outcome.value.nextCursor)
        : appendPage(valueToInventoryIdOptions(), rows, outcome.value.nextCursor),
    );
  };
  const readValueToInventoryIdRecord = async (key: string): Promise<InventoryQueryRow | null> => {
    const request = writeMember({}, ["id"], key) as InventoryGetRequest;
    const outcome = await inventoryGet(props.transport, [request]);
    return outcome.status === "completed" ? ((outcome.value ?? null) as unknown as InventoryQueryRow | null) : null;
  };
  void readValueToInventoryIdOptions(null);

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
        <form.Field name={`value.fromInventoryId`}>
          {(field) => (
            <RecordSelect
              label="from inventory id"
              options={valueFromInventoryIdOptions().rows}
              optionValue={(row) => String(row.id)}
              optionLabel={(row) => String(row.disposition)}
              value={field().state.value == null ? null : String(field().state.value)}
              onChange={(value) => field().handleChange(value ?? "")}
              hasNextPage={hasNextPage(valueFromInventoryIdOptions())}
              onNextPage={() => void readValueFromInventoryIdOptions(valueFromInventoryIdOptions().cursor)}
              readRow={readValueFromInventoryIdRecord}
              error={refusalMarks(refusal()?.member ?? null, "value.from_inventory_id") ? (refusal()?.text ?? null) : null}
            />
          )}
        </form.Field>
        <form.Field name={`value.toInventoryId`}>
          {(field) => (
            <RecordSelect
              label="to inventory id"
              options={valueToInventoryIdOptions().rows}
              optionValue={(row) => String(row.id)}
              optionLabel={(row) => String(row.disposition)}
              value={field().state.value == null ? null : String(field().state.value)}
              onChange={(value) => field().handleChange(value ?? "")}
              hasNextPage={hasNextPage(valueToInventoryIdOptions())}
              onNextPage={() => void readValueToInventoryIdOptions(valueToInventoryIdOptions().cursor)}
              readRow={readValueToInventoryIdRecord}
              error={refusalMarks(refusal()?.member ?? null, "value.to_inventory_id") ? (refusal()?.text ?? null) : null}
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
      inventoryId: z.string().regex(UUID_TEXT, "expected a UUID"),
      toLocationId: z.string().regex(UUID_TEXT, "expected a UUID"),
      toPackagingId: z.string().regex(UUID_TEXT, "expected a UUID"),
    })
    .optional(),
});

/** What the form for `wamn-wms:inventory/move@1.0.0` can start with. */
export interface InventoryMoveFormInitial {
  value?: {
    inventoryId?: Uuid;
    toLocationId?: Uuid;
    toPackagingId?: Uuid;
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
  const [valueInventoryIdOptions, setValueInventoryIdOptions] = createSignal<PageState<InventoryQueryRow>>(emptyPage<InventoryQueryRow>());
  const readValueInventoryIdOptions = async (cursor: string | null) => {
    let request = {} as InventoryQueryRequest;
    if (cursor !== null) {
      request = writeMember(request, ["cursor"], cursor) as InventoryQueryRequest;
    }
    const outcome = await inventoryQuery(props.transport, [request]);
    if (outcome.status !== "completed") {
      return;
    }
    const rows = outcome.value.item as InventoryQueryRow[];
    setValueInventoryIdOptions(
      cursor === null
        ? firstPage(rows, outcome.value.nextCursor)
        : appendPage(valueInventoryIdOptions(), rows, outcome.value.nextCursor),
    );
  };
  const readValueInventoryIdRecord = async (key: string): Promise<InventoryQueryRow | null> => {
    const request = writeMember({}, ["id"], key) as InventoryGetRequest;
    const outcome = await inventoryGet(props.transport, [request]);
    return outcome.status === "completed" ? ((outcome.value ?? null) as unknown as InventoryQueryRow | null) : null;
  };
  void readValueInventoryIdOptions(null);
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
  const [valueToPackagingIdOptions, setValueToPackagingIdOptions] = createSignal<PageState<PackagingQueryRow>>(emptyPage<PackagingQueryRow>());
  const readValueToPackagingIdOptions = async (cursor: string | null) => {
    let request = {} as PackagingQueryRequest;
    if (cursor !== null) {
      request = writeMember(request, ["cursor"], cursor) as PackagingQueryRequest;
    }
    const outcome = await packagingQuery(props.transport, [request]);
    if (outcome.status !== "completed") {
      return;
    }
    const rows = outcome.value.item as PackagingQueryRow[];
    setValueToPackagingIdOptions(
      cursor === null
        ? firstPage(rows, outcome.value.nextCursor)
        : appendPage(valueToPackagingIdOptions(), rows, outcome.value.nextCursor),
    );
  };
  const readValueToPackagingIdRecord = async (key: string): Promise<PackagingQueryRow | null> => {
    const request = writeMember({}, ["id"], key) as PackagingGetRequest;
    const outcome = await packagingGet(props.transport, [request]);
    return outcome.status === "completed" ? ((outcome.value ?? null) as unknown as PackagingQueryRow | null) : null;
  };
  void readValueToPackagingIdOptions(null);

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
        <form.Field name={`value.inventoryId`}>
          {(field) => (
            <RecordSelect
              label="inventory id"
              options={valueInventoryIdOptions().rows}
              optionValue={(row) => String(row.id)}
              optionLabel={(row) => String(row.disposition)}
              value={field().state.value == null ? null : String(field().state.value)}
              onChange={(value) => field().handleChange(value ?? "")}
              hasNextPage={hasNextPage(valueInventoryIdOptions())}
              onNextPage={() => void readValueInventoryIdOptions(valueInventoryIdOptions().cursor)}
              readRow={readValueInventoryIdRecord}
              error={refusalMarks(refusal()?.member ?? null, "value.inventory_id") ? (refusal()?.text ?? null) : null}
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
        <form.Field name={`value.toPackagingId`}>
          {(field) => (
            <RecordSelect
              label="to packaging id"
              options={valueToPackagingIdOptions().rows}
              optionValue={(row) => String(row.id)}
              optionLabel={(row) => String(row.code)}
              value={field().state.value == null ? null : String(field().state.value)}
              onChange={(value) => field().handleChange(value ?? "")}
              hasNextPage={hasNextPage(valueToPackagingIdOptions())}
              onNextPage={() => void readValueToPackagingIdOptions(valueToPackagingIdOptions().cursor)}
              readRow={readValueToPackagingIdRecord}
              error={refusalMarks(refusal()?.member ?? null, "value.to_packaging_id") ? (refusal()?.text ?? null) : null}
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

/** What the table for `wamn-wms:inventory/query@1.0.0` takes. */
export interface InventoryQueryTableProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** Input the parent fixes, which the operator does not edit. */
  readonly fixed?: Partial<InventoryQueryRequest>;
  /** Called when the operator picks one row. */
  readonly onRowSelect?: (row: InventoryQueryRow) => void;
  /** Called when the operator opens `wamn-wms:inventory/get@1.0.0` from one row. */
  readonly onOpenInventoryGet?: (row: InventoryQueryRow) => void;
  /** Called with the values one row hands to `wamn-wms:inventory/adjust@1.0.0`. */
  readonly onFillInventoryAdjust?: (initial: InventoryAdjustFormInitial) => void;
  /** Called with the values one row hands to `wamn-wms:inventory/move@1.0.0`. */
  readonly onFillInventoryMove?: (initial: InventoryMoveFormInitial) => void;
  /** Called with the values one row hands to `wamn-wms:inventory/split@1.0.0`. */
  readonly onFillInventorySplit?: (initial: InventorySplitFormInitial) => void;
  /** Called with every outcome this screen reads. */
  readonly onOutcome?: (outcome: Outcome<InventoryQueryResult>) => void;
}

/** What an operator calls this screen. The page decides where it goes. */
export const InventoryQueryTableLabel = "query";

/**
 * The table for `wamn-wms:inventory/query@1.0.0`.
 *
 * It owns its page controls and its rows. A change to a control clears the
 * rows, because a cursor names a position in the list the old input produced.
 *
 * It reads when the operator asks, and not when it mounts, because a read is
 * a request that the operator did not send yet.
 */
export function InventoryQueryTable(props: InventoryQueryTableProps) {
  const [controls, setControls] = createSignal<Partial<InventoryQueryRequest>>({});
  const [page, setPage] = createSignal<PageState<InventoryQueryRow>>(emptyPage<InventoryQueryRow>());

  const read = async (cursor: string | null) => {
    setPage(startRead(page()));
    const request = {
      ...controls(),
      ...props.fixed,
    } as InventoryQueryRequest;
    const sent = cursor === null ? request : (writeMember(request, ["cursor"], cursor) as InventoryQueryRequest);
    const outcome = await query(props.transport, [sent]);
    props.onOutcome?.(outcome);
    if (outcome.status !== "completed") {
      announceOutcome(outcome, InventoryQueryTableLabel);
      setPage(failedRead(page(), outcome));
      return;
    }
    const rows = outcome.value.item;
    setPage(cursor === null ? firstPage(rows, outcome.value.nextCursor) : appendPage(page(), rows, outcome.value.nextCursor));
  };

  const restart = () => {
    setPage(emptyPage<InventoryQueryRow>());
    void read(null);
  };

  const change = (path: readonly string[], value: JsonValue) => {
    setControls((current) => writeControl(current, path, value));
    restart();
  };

  const locationGetLabels = createRecordLabels(async (key) => {
    const request = writeMember({}, ["id"], key) as LocationGetRequest;
    const outcome = await locationGet(props.transport, [request]);
    if (outcome.status !== "completed") {
      return null;
    }
    const text = outcome.value.locationCode;
    return text == null ? null : String(text);
  });
  const packagingGetLabels = createRecordLabels(async (key) => {
    const request = writeMember({}, ["id"], key) as PackagingGetRequest;
    const outcome = await packagingGet(props.transport, [request]);
    if (outcome.status !== "completed") {
      return null;
    }
    const text = outcome.value.code;
    return text == null ? null : String(text);
  });
  const productGetLabels = createRecordLabels(async (key) => {
    const request = writeMember({}, ["id"], key) as ProductGetRequest;
    const outcome = await productGet(props.transport, [request]);
    if (outcome.status !== "completed") {
      return null;
    }
    const text = outcome.value.productCode;
    return text == null ? null : String(text);
  });

  const columns: ColumnDef<GridFeatures, InventoryQueryRow>[] = [
    {
      accessorKey: "createdAt",
      header: "created at",
      cell: (cell) => cellText(cell.getValue() as JsonValue, "timestamptz"),
    },
    {
      accessorKey: "disposition",
      header: "disposition",
      cell: (cell) => (
        <Show when={cellText(cell.getValue() as JsonValue, "text") !== ""}>
          <Badge variant="outline">{cellText(cell.getValue() as JsonValue, "text")}</Badge>
        </Show>
      ),
    },
    {
      accessorKey: "id",
      header: "id",
      cell: (cell) => cellText(cell.getValue() as JsonValue, "uuid"),
    },
    {
      accessorKey: "lifecycle",
      header: "lifecycle",
      cell: (cell) => (
        <Show when={cellText(cell.getValue() as JsonValue, "text") !== ""}>
          <Badge variant="outline">{cellText(cell.getValue() as JsonValue, "text")}</Badge>
        </Show>
      ),
    },
    {
      accessorKey: "locationId",
      header: "location id",
      cell: (cell) => <>{locationGetLabels(cell.getValue() as string | null)}</>,
    },
    {
      accessorKey: "packagingId",
      header: "packaging id",
      cell: (cell) => <>{packagingGetLabels(cell.getValue() as string | null)}</>,
    },
    {
      accessorKey: "productId",
      header: "product id",
      cell: (cell) => <>{productGetLabels(cell.getValue() as string | null)}</>,
    },
    {
      accessorKey: "quantity",
      header: "quantity",
      cell: (cell) => cellText(cell.getValue() as JsonValue, "numeric"),
    },
    {
      accessorKey: "rowVersion",
      header: "row version",
      cell: (cell) => cellText(cell.getValue() as JsonValue, "int32"),
    },
    {
      id: "openInventoryGet",
      header: "",
      cell: (cell) => (
        <Show when={props.onOpenInventoryGet}>
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => props.onOpenInventoryGet?.(cell.row.original)}
          >
            get
          </Button>
        </Show>
      ),
    },
    {
      id: "fillInventoryAdjust",
      header: "",
      cell: (cell) => (
        <Show when={props.onFillInventoryAdjust}>
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => props.onFillInventoryAdjust?.(writeMember({} as InventoryAdjustFormInitial, ["value", "inventoryId"], cell.row.original.id))}
          >
            adjust
          </Button>
        </Show>
      ),
    },
    {
      id: "fillInventoryMove",
      header: "",
      cell: (cell) => (
        <Show when={props.onFillInventoryMove}>
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => props.onFillInventoryMove?.(writeMember({} as InventoryMoveFormInitial, ["value", "inventoryId"], cell.row.original.id))}
          >
            move
          </Button>
        </Show>
      ),
    },
    {
      id: "fillInventorySplit",
      header: "",
      cell: (cell) => (
        <Show when={props.onFillInventorySplit}>
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => props.onFillInventorySplit?.(writeMember({} as InventorySplitFormInitial, ["value", "fromInventoryId"], cell.row.original.id))}
          >
            split
          </Button>
        </Show>
      ),
    },
  ];

  const table = createTable({
    features: gridFeatures,
    get data() {
      return page().rows as InventoryQueryRow[];
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
          <DataGridTable />
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

/** What an operator types for `wamn-wms:inventory/split@1.0.0`. */
const SPLIT_INPUT = z.object({
  value: z
    .object({
      fromInventoryId: z.string().regex(UUID_TEXT, "expected a UUID"),
      quantity: z.string().regex(NUMERIC_TEXT, "expected decimal text"),
      toLocationId: z.string().regex(UUID_TEXT, "expected a UUID"),
      toPackagingId: z.string().regex(UUID_TEXT, "expected a UUID"),
    })
    .optional(),
});

/** What the form for `wamn-wms:inventory/split@1.0.0` can start with. */
export interface InventorySplitFormInitial {
  value?: {
    fromInventoryId?: Uuid;
    quantity?: Numeric;
    toLocationId?: Uuid;
    toPackagingId?: Uuid;
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
  const [valueFromInventoryIdOptions, setValueFromInventoryIdOptions] = createSignal<PageState<InventoryQueryRow>>(emptyPage<InventoryQueryRow>());
  const readValueFromInventoryIdOptions = async (cursor: string | null) => {
    let request = {} as InventoryQueryRequest;
    if (cursor !== null) {
      request = writeMember(request, ["cursor"], cursor) as InventoryQueryRequest;
    }
    const outcome = await inventoryQuery(props.transport, [request]);
    if (outcome.status !== "completed") {
      return;
    }
    const rows = outcome.value.item as InventoryQueryRow[];
    setValueFromInventoryIdOptions(
      cursor === null
        ? firstPage(rows, outcome.value.nextCursor)
        : appendPage(valueFromInventoryIdOptions(), rows, outcome.value.nextCursor),
    );
  };
  const readValueFromInventoryIdRecord = async (key: string): Promise<InventoryQueryRow | null> => {
    const request = writeMember({}, ["id"], key) as InventoryGetRequest;
    const outcome = await inventoryGet(props.transport, [request]);
    return outcome.status === "completed" ? ((outcome.value ?? null) as unknown as InventoryQueryRow | null) : null;
  };
  void readValueFromInventoryIdOptions(null);
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
  const [valueToPackagingIdOptions, setValueToPackagingIdOptions] = createSignal<PageState<PackagingQueryRow>>(emptyPage<PackagingQueryRow>());
  const readValueToPackagingIdOptions = async (cursor: string | null) => {
    let request = {} as PackagingQueryRequest;
    if (cursor !== null) {
      request = writeMember(request, ["cursor"], cursor) as PackagingQueryRequest;
    }
    const outcome = await packagingQuery(props.transport, [request]);
    if (outcome.status !== "completed") {
      return;
    }
    const rows = outcome.value.item as PackagingQueryRow[];
    setValueToPackagingIdOptions(
      cursor === null
        ? firstPage(rows, outcome.value.nextCursor)
        : appendPage(valueToPackagingIdOptions(), rows, outcome.value.nextCursor),
    );
  };
  const readValueToPackagingIdRecord = async (key: string): Promise<PackagingQueryRow | null> => {
    const request = writeMember({}, ["id"], key) as PackagingGetRequest;
    const outcome = await packagingGet(props.transport, [request]);
    return outcome.status === "completed" ? ((outcome.value ?? null) as unknown as PackagingQueryRow | null) : null;
  };
  void readValueToPackagingIdOptions(null);

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
        <form.Field name={`value.fromInventoryId`}>
          {(field) => (
            <RecordSelect
              label="from inventory id"
              options={valueFromInventoryIdOptions().rows}
              optionValue={(row) => String(row.id)}
              optionLabel={(row) => String(row.disposition)}
              value={field().state.value == null ? null : String(field().state.value)}
              onChange={(value) => field().handleChange(value ?? "")}
              hasNextPage={hasNextPage(valueFromInventoryIdOptions())}
              onNextPage={() => void readValueFromInventoryIdOptions(valueFromInventoryIdOptions().cursor)}
              readRow={readValueFromInventoryIdRecord}
              error={refusalMarks(refusal()?.member ?? null, "value.from_inventory_id") ? (refusal()?.text ?? null) : null}
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
        <form.Field name={`value.toPackagingId`}>
          {(field) => (
            <RecordSelect
              label="to packaging id"
              options={valueToPackagingIdOptions().rows}
              optionValue={(row) => String(row.id)}
              optionLabel={(row) => String(row.code)}
              value={field().state.value == null ? null : String(field().state.value)}
              onChange={(value) => field().handleChange(value ?? "")}
              hasNextPage={hasNextPage(valueToPackagingIdOptions())}
              onNextPage={() => void readValueToPackagingIdOptions(valueToPackagingIdOptions().cursor)}
              readRow={readValueToPackagingIdRecord}
              error={refusalMarks(refusal()?.member ?? null, "value.to_packaging_id") ? (refusal()?.text ?? null) : null}
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
