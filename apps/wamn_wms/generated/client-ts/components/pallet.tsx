// @generated from the client-contract IR; do not edit.
//
// `pallet` components. Each one calls the bindings and the runtime, and
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
  newIdempotencyKey,
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
  ChoiceField,
  DataTable,
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
  createTableLoad,
  type DataTableScopeFilter,
} from "@wamn/ui";
import {
  PALLET_CREATE_REQUEST_FIELDS,
  create,
  get,
  query,
  type PalletCreateRequest,
  type PalletCreateResult,
  type PalletGetRequest,
  type PalletGetResult,
  type PalletQueryRequest,
  type PalletQueryResult,
  type PalletQueryRow,
} from "../pallet.js";
import {
  type InventoryAdjustFormInitial,
  type InventoryMoveFormInitial,
  type InventorySplitFormInitial,
} from "./inventory.js";
import {
  get as locationGet,
  query as locationQuery,
  type LocationGetRequest,
  type LocationQueryRequest,
  type LocationQueryRow,
} from "../location.js";

/** What the release accepts: one UUID, hyphenated. */
const UUID_TEXT = /^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$/;

/** What an operator types for `wamn-wms:pallet/create@1.0.0`. */
const CREATE_INPUT = z.object({
  locationId: z.string().regex(UUID_TEXT, "expected a UUID"),
  palletCode: z.string(),
  status: z.enum(["available", "held"]),
});

/** What the form for `wamn-wms:pallet/create@1.0.0` can start with. */
export interface PalletCreateFormInitial {
  locationId?: Uuid;
  palletCode?: string;
  status?: "available" | "held";
}

/** What the form for `wamn-wms:pallet/create@1.0.0` takes. */
export interface PalletCreateFormProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** Values the form starts with. */
  readonly initial?: PalletCreateFormInitial;
  /** Called with the outcome of every submission. */
  readonly onSubmitted?: (outcome: Outcome<PalletCreateResult>) => void;
}

/** What an operator calls this screen. The page decides where it goes. */
export const PalletCreateFormLabel = "create";

/**
 * The form for `wamn-wms:pallet/create@1.0.0`.
 *
 * It renders what the operator fills and nothing else. The reserved inputs
 * come from the runtime at submit time, and the operator never sees them.
 */
export function PalletCreateForm(props: PalletCreateFormProps) {
  const [refusal, setRefusal] = createSignal<{ text: string; member: string | null } | null>(null);
  const [done, setDone] = createSignal(false);

  const form = createForm(() => ({
    defaultValues: { ...props.initial } as Partial<PalletCreateRequest>,
    onSubmit: async ({ value }: { value: Partial<PalletCreateRequest> }) => {
      setDone(false);
      const checked = CREATE_INPUT.safeParse(value);
      if (!checked.success) {
        const issue = checked.error.issues[0];
        setRefusal({
          text: issue?.message ?? "A value is not valid.",
          member: checkedMember(
            issue?.path as (string | number)[] | undefined,
            PALLET_CREATE_REQUEST_FIELDS,
          ),
        });
        return;
      }
      let item = { ...value } as PalletCreateRequest;
      item = writeMember(item, ["idempotencyKey"], newIdempotencyKey());
      item = writeMember(item, ["requestId"], newRequestId());
      const outcome = await create(props.transport, [item]);
      props.onSubmitted?.(outcome);
      announceOutcome(outcome, PalletCreateFormLabel);
      setRefusal(
        outcome.status === "refused"
          ? { text: refusalSentence(outcome.code, outcome.text), member: refusedMember(outcome.detail) }
          : null,
      );
      setDone(outcome.status === "completed");
    },
  }));
  const [locationIdOptions, setLocationIdOptions] = createSignal<PageState<LocationQueryRow>>(emptyPage<LocationQueryRow>());
  const [locationIdSearch, setLocationIdSearch] = createSignal("");
  const readLocationIdOptions = async (cursor: string | null) => {
    let request = {} as LocationQueryRequest;
    if (locationIdSearch() !== "") {
      request = writeMember(request, ["filter", "locationCode"], [locationIdSearch()]) as LocationQueryRequest;
    }
    if (cursor !== null) {
      request = writeMember(request, ["cursor"], cursor) as LocationQueryRequest;
    }
    const outcome = await locationQuery(props.transport, [request]);
    if (outcome.status !== "completed") {
      return;
    }
    const rows = outcome.value.item as LocationQueryRow[];
    setLocationIdOptions(
      cursor === null
        ? firstPage(rows, outcome.value.nextCursor)
        : appendPage(locationIdOptions(), rows, outcome.value.nextCursor),
    );
  };
  const readLocationIdRecord = async (key: string): Promise<LocationQueryRow | null> => {
    const request = writeMember({}, ["id"], key) as LocationGetRequest;
    const outcome = await locationGet(props.transport, [request]);
    return outcome.status === "completed" ? ((outcome.value ?? null) as unknown as LocationQueryRow | null) : null;
  };
  void readLocationIdOptions(null);
  onCleanup(afterWrites(props.transport, () => void readLocationIdOptions(null)));

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
        <form.Field name={`locationId`}>
          {(field) => (
            <RecordSelect
              label="location id"
              options={locationIdOptions().rows}
              optionValue={(row) => String(row.id)}
              optionLabel={(row) => String(row.locationCode)}
              value={field().state.value == null ? null : String(field().state.value)}
              onChange={(value) => field().handleChange(value ?? "")}
              onSearch={(text) => {
                setLocationIdSearch(text);
                void readLocationIdOptions(null);
              }}
              hasNextPage={hasNextPage(locationIdOptions())}
              onNextPage={() => void readLocationIdOptions(locationIdOptions().cursor)}
              readRow={readLocationIdRecord}
              error={refusalMarks(refusal()?.member ?? null, "location_id") ? (refusal()?.text ?? null) : null}
            />
          )}
        </form.Field>
        <form.Field name={`palletCode`}>
          {(field) => (
            <TextField
              label="pallet code"
              type="text"
              value={String(field().state.value ?? "")}
              onInput={(value) => field().handleChange(value)}
              error={refusalMarks(refusal()?.member ?? null, "pallet_code") ? (refusal()?.text ?? null) : null}
            />
          )}
        </form.Field>
        <form.Field name={`status`}>
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
              error={refusalMarks(refusal()?.member ?? null, "status") ? (refusal()?.text ?? null) : null}
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

/** The record that the detail for `wamn-wms:pallet/get@1.0.0` reads. */
export interface PalletGetDetailInput {
  readonly id: Uuid;
}

/** What the detail screen for `wamn-wms:pallet/get@1.0.0` takes. */
export interface PalletGetDetailProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** The input that names the record. */
  readonly input: PalletGetDetailInput;
  /** Called with every outcome this screen reads. */
  readonly onOutcome?: (outcome: Outcome<PalletGetResult>) => void;
}

/** What an operator calls this screen. The page decides where it goes. */
export const PalletGetDetailLabel = "get";

/**
 * The detail screen for `wamn-wms:pallet/get@1.0.0`.
 *
 * It reads when it mounts and again whenever its input changes, because the
 * input names the record it shows.
 */
export function PalletGetDetail(props: PalletGetDetailProps) {
  const [outcome, { refetch: readAgain }] = createResource(
    () => props.input,
    async (input: PalletGetDetailInput) => {
      const read = await get(props.transport, [
        input as PalletGetRequest,
      ]);
      props.onOutcome?.(read);
      if (read.status !== "completed") {
        announceOutcome(read, PalletGetDetailLabel);
      }
      return read;
    },
  );
  onCleanup(afterWrites(props.transport, () => void readAgain()));
  const record = (): PalletGetResult | undefined => {
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
        <DetailItem term="created by">{cellText(readMember(record(), ["createdBy"]), "uuid")}</DetailItem>
        <DetailItem term="id">{cellText(readMember(record(), ["id"]), "uuid")}</DetailItem>
        <DetailItem term="location id">{cellText(readMember(record(), ["locationId"]), "uuid")}</DetailItem>
        <DetailItem term="pallet code">{cellText(readMember(record(), ["palletCode"]), "text")}</DetailItem>
        <DetailItem term="row version">{cellText(readMember(record(), ["rowVersion"]), "int32")}</DetailItem>
        <DetailItem term="status">{cellText(readMember(record(), ["status"]), "text")}</DetailItem>
        <DetailItem term="updated at">{cellText(readMember(record(), ["updatedAt"]), "timestamptz")}</DetailItem>
        <DetailItem term="updated by">{cellText(readMember(record(), ["updatedBy"]), "uuid")}</DetailItem>
      </DetailList>
    </section>
  );
}

/** What the table for `wamn-wms:pallet/query@1.0.0` takes. */
export interface PalletQueryTableProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** Input the parent fixes, which the operator does not edit. */
  readonly fixed?: Partial<PalletQueryRequest>;
  /** Called when the operator opens `wamn-wms:pallet/get@1.0.0` from one row. */
  readonly onOpenPalletGet?: (row: PalletQueryRow) => void;
  /** Called with the values one row hands to `wamn-wms:inventory/adjust@1.0.0`. */
  readonly onFillInventoryAdjust?: (initial: InventoryAdjustFormInitial) => void;
  /** Called with the values one row hands to `wamn-wms:inventory/move@1.0.0`. */
  readonly onFillInventoryMove?: (initial: InventoryMoveFormInitial) => void;
  /** Called with the values one row hands to `wamn-wms:inventory/split@1.0.0`. */
  readonly onFillInventorySplit?: (initial: InventorySplitFormInitial) => void;
  /** Called with every outcome this screen reads. */
  readonly onOutcome?: (outcome: Outcome<PalletQueryResult>) => void;
}

/** What an operator calls this screen. The page decides where it goes. */
export const PalletQueryTableLabel = "query";

/**
 * The table for `wamn-wms:pallet/query@1.0.0`: the DataTable over `PALLET_QUERY_TABLE`.
 *
 * It loads when it mounts. A change to a filter, a sort of rows the load did
 * not read in full, a cap change and a refresh each start a new load.
 */
export function PalletQueryTable(props: PalletQueryTableProps) {
  const [scope, setScope] = createSignal<Partial<PalletQueryRequest>>({});
  const load = createTableLoad<PalletQueryRow>(PALLET_QUERY_TABLE, async (limit, sort) => {
    let request = { ...scope(), ...props.fixed } as PalletQueryRequest;
    request = writeMember(request, ["limit"], limit) as PalletQueryRequest;
    if (sort !== undefined) {
      request = writeMember(request, ["sort", "field"], sort.field) as PalletQueryRequest;
      request = writeMember(request, ["sort", "direction"], sort.direction) as PalletQueryRequest;
    }
    const outcome = await query(props.transport, [request]);
    props.onOutcome?.(outcome);
    if (outcome.status !== "completed") {
      announceOutcome(outcome, PalletQueryTableLabel);
    }
    return outcome;
  });
  void load.load();
  onCleanup(afterWrites(props.transport, () => void load.load()));

  const changeScope = (filters: readonly DataTableScopeFilter[]) => {
    let next: Partial<PalletQueryRequest> = {};
    for (const filter of filters) {
      switch (filter.field) {
        case "locationId":
          next = writeMember(next, ["filter", "locationId"], [...filter.values]);
          break;
        case "palletCode":
          next = writeMember(next, ["filter", "palletCode"], [...filter.values]);
          break;
        case "status":
          next = writeMember(next, ["filter", "status"], [...filter.values]);
          break;
      }
    }
    setScope(next);
    void load.load();
  };

  const locationGetLabels = createRecordLabels(props.transport, async (key) => {
    const request = writeMember({}, ["id"], key) as LocationGetRequest;
    const outcome = await locationGet(props.transport, [request]);
    if (outcome.status !== "completed") {
      return null;
    }
    const text = outcome.value.locationCode;
    return text == null ? null : String(text);
  });

  const columns = PALLET_QUERY_TABLE.columns.map((column) => {
    switch (column.field) {
      case "locationId":
        return { ...column, cell: (value: unknown) => <>{locationGetLabels(value as string | null)}</> };
      default:
        return column;
    }
  });

  const actions = (row: PalletQueryRow) => (
    <>
      <Show when={props.onOpenPalletGet}>
        <Button type="button" variant="outline" size="sm" onClick={() => props.onOpenPalletGet?.(row)}>
          get
        </Button>
      </Show>
      <Show when={props.onFillInventoryAdjust}>
        <Button
          type="button"
          variant="outline"
          size="sm"
          onClick={() => props.onFillInventoryAdjust?.(writeMember({} as InventoryAdjustFormInitial, ["value", "palletId"], row.id))}
        >
          adjust
        </Button>
      </Show>
      <Show when={props.onFillInventoryMove}>
        <Button
          type="button"
          variant="outline"
          size="sm"
          onClick={() => props.onFillInventoryMove?.(writeMember({} as InventoryMoveFormInitial, ["value", "palletId"], row.id))}
        >
          move
        </Button>
      </Show>
      <Show when={props.onFillInventorySplit}>
        <Button
          type="button"
          variant="outline"
          size="sm"
          onClick={() => props.onFillInventorySplit?.(writeMember({} as InventorySplitFormInitial, ["value", "sourcePalletId"], row.id))}
        >
          split
        </Button>
      </Show>
    </>
  );

  return (
    <TableScreen>
      <DataTable
        name="pallet"
        columns={columns}
        rowId={PALLET_QUERY_TABLE.rowId}
        rows={load.state().rows}
        fullyRead={load.state().fullyRead}
        busy={load.state().busy}
        refusal={load.state().refusal}
        cap={load.state().cap}
        onCapChange={(cap) => void load.load(cap)}
        onRefresh={() => void load.load()}
        startedAt={load.state().startedAt}
        endedAt={load.state().endedAt}
        sortFields={PALLET_QUERY_TABLE.sortFields}
        sortMaxFields={PALLET_QUERY_TABLE.sortMaxFields}
        onSortChange={load.sortBy}
        scopeFilters={PALLET_QUERY_TABLE.scopeFilters}
        onScopeChange={changeScope}
        rowActions={actions}
      />
    </TableScreen>
  );
}

/** The table definition of `wamn-wms:pallet/query@1.0.0`. */
export const PALLET_QUERY_TABLE = {
  read: "query",
  rowId: ["id"],
  pageMaximum: 100,
  scopeFilters: ["locationId", "palletCode", "status"],
  sortFields: [{ field: "createdAt", wire: "created_at" }, { field: "locationId", wire: "location_id" }, { field: "palletCode", wire: "pallet_code" }, { field: "updatedAt", wire: "updated_at" }],
  sortDirections: ["ascending", "descending"],
  sortMaxFields: 1,
  columns: [
    { field: "createdAt", label: "created at", type: "timestamptz", role: "value" },
    { field: "createdBy", label: "created by", type: "uuid", role: "value" },
    { field: "id", label: "id", type: "uuid", role: "key" },
    { field: "locationId", label: "location id", type: "uuid", role: "reference", displayField: "locationCode" },
    { field: "palletCode", label: "pallet code", type: "text", role: "value" },
    { field: "rowVersion", label: "row version", type: "int32", role: "revision" },
    { field: "status", label: "status", type: "text", role: "value" },
    { field: "updatedAt", label: "updated at", type: "timestamptz", role: "value" },
    { field: "updatedBy", label: "updated by", type: "uuid", role: "value" },
  ],
  actions: [
    { operation: "wamn-wms:pallet/get@1.0.0", label: "get", many: false },
    { operation: "wamn-wms:inventory/adjust@1.0.0", label: "adjust", many: true },
    { operation: "wamn-wms:inventory/move@1.0.0", label: "move", many: true },
    { operation: "wamn-wms:inventory/split@1.0.0", label: "split", many: true },
  ],
  childTables: [],
} as const;
