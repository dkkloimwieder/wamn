// @generated from the client-contract IR; do not edit.
//
// `location` components. Each one calls the bindings and the runtime, and
// nothing else.

import { Show, createResource, createSignal, onCleanup } from "solid-js";
import { createForm } from "@tanstack/solid-form";
import { z } from "zod";
import {
  afterWrites,
  cellText,
  checkedMember,
  newIdempotencyKey,
  newRequestId,
  readMember,
  refusalMarks,
  refusalSentence,
  refusedMember,
  type Outcome,
  type Transport,
  type Uuid,
  writeMember,
} from "@wamn/web-runtime";
import {
  Button,
  DataTable,
  DetailItem,
  DetailList,
  FieldError,
  FieldGroup,
  FormActions,
  FormDone,
  TableScreen,
  TextField,
  announceOutcome,
  createTableLoad,
  type DataTableScopeFilter,
} from "@wamn/ui";
import {
  LOCATION_CREATE_REQUEST_FIELDS,
  LOCATION_UPDATE_REQUEST_FIELDS,
  create,
  get,
  query,
  type LocationCreateRequest,
  type LocationCreateResult,
  type LocationGetRequest,
  type LocationGetResult,
  type LocationQueryRequest,
  type LocationQueryResult,
  type LocationQueryRow,
  type LocationUpdateRequest,
  type LocationUpdateResult,
  update,
} from "../location.js";
import {
  type InventoryMoveFormInitial,
  type InventorySplitFormInitial,
} from "./inventory.js";
import {
  type PalletCreateFormInitial,
} from "./pallet.js";

/** What an operator types for `wamn-wms:location/create@1.0.0`. */
const CREATE_INPUT = z.object({
  locationCode: z.string(),
});

/** What the form for `wamn-wms:location/create@1.0.0` can start with. */
export interface LocationCreateFormInitial {
  locationCode?: string;
}

/** What the form for `wamn-wms:location/create@1.0.0` takes. */
export interface LocationCreateFormProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** Values the form starts with. */
  readonly initial?: LocationCreateFormInitial;
  /** Called with the outcome of every submission. */
  readonly onSubmitted?: (outcome: Outcome<LocationCreateResult>) => void;
}

/** What an operator calls this screen. The page decides where it goes. */
export const LocationCreateFormLabel = "create";

/**
 * The form for `wamn-wms:location/create@1.0.0`.
 *
 * It renders what the operator fills and nothing else. The reserved inputs
 * come from the runtime at submit time, and the operator never sees them.
 */
export function LocationCreateForm(props: LocationCreateFormProps) {
  const [refusal, setRefusal] = createSignal<{ text: string; member: string | null } | null>(null);
  const [done, setDone] = createSignal(false);

  const form = createForm(() => ({
    defaultValues: { ...props.initial } as Partial<LocationCreateRequest>,
    onSubmit: async ({ value }: { value: Partial<LocationCreateRequest> }) => {
      setDone(false);
      const checked = CREATE_INPUT.safeParse(value);
      if (!checked.success) {
        const issue = checked.error.issues[0];
        setRefusal({
          text: issue?.message ?? "A value is not valid.",
          member: checkedMember(
            issue?.path as (string | number)[] | undefined,
            LOCATION_CREATE_REQUEST_FIELDS,
          ),
        });
        return;
      }
      let item = { ...value } as LocationCreateRequest;
      item = writeMember(item, ["idempotencyKey"], newIdempotencyKey());
      item = writeMember(item, ["requestId"], newRequestId());
      const outcome = await create(props.transport, [item]);
      props.onSubmitted?.(outcome);
      announceOutcome(outcome, LocationCreateFormLabel);
      setRefusal(
        outcome.status === "refused"
          ? { text: refusalSentence(outcome.code, outcome.text), member: refusedMember(outcome.detail) }
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
        <form.Field name={`locationCode`}>
          {(field) => (
            <TextField
              label="location code"
              type="text"
              value={String(field().state.value ?? "")}
              onInput={(value) => field().handleChange(value)}
              error={refusalMarks(refusal()?.member ?? null, "location_code") ? (refusal()?.text ?? null) : null}
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

/** The record that the detail for `wamn-wms:location/get@1.0.0` reads. */
export interface LocationGetDetailInput {
  readonly id: Uuid;
}

/** What the detail screen for `wamn-wms:location/get@1.0.0` takes. */
export interface LocationGetDetailProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** The input that names the record. */
  readonly input: LocationGetDetailInput;
  /** Called with every outcome this screen reads. */
  readonly onOutcome?: (outcome: Outcome<LocationGetResult>) => void;
}

/** What an operator calls this screen. The page decides where it goes. */
export const LocationGetDetailLabel = "get";

/**
 * The detail screen for `wamn-wms:location/get@1.0.0`.
 *
 * It reads when it mounts and again whenever its input changes, because the
 * input names the record it shows.
 */
export function LocationGetDetail(props: LocationGetDetailProps) {
  const [outcome, { refetch: readAgain }] = createResource(
    () => props.input,
    async (input: LocationGetDetailInput) => {
      const read = await get(props.transport, [
        input as LocationGetRequest,
      ]);
      props.onOutcome?.(read);
      if (read.status !== "completed") {
        announceOutcome(read, LocationGetDetailLabel);
      }
      return read;
    },
  );
  onCleanup(afterWrites(props.transport, () => void readAgain()));
  const record = (): LocationGetResult | undefined => {
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
        <DetailItem term="id">{cellText(readMember(record(), ["id"]), "uuid")}</DetailItem>
        <DetailItem term="location code">{cellText(readMember(record(), ["locationCode"]), "text")}</DetailItem>
        <DetailItem term="row version">{cellText(readMember(record(), ["rowVersion"]), "int32")}</DetailItem>
      </DetailList>
    </section>
  );
}

/** What the table for `wamn-wms:location/query@1.0.0` takes. */
export interface LocationQueryTableProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** Input the parent fixes, which the operator does not edit. */
  readonly fixed?: Partial<LocationQueryRequest>;
  /** Called when the operator opens `wamn-wms:location/get@1.0.0` from one row. */
  readonly onOpenLocationGet?: (row: LocationQueryRow) => void;
  /** Called with the values one row hands to `wamn-wms:inventory/move@1.0.0`. */
  readonly onFillInventoryMove?: (initial: InventoryMoveFormInitial) => void;
  /** Called with the values one row hands to `wamn-wms:inventory/split@1.0.0`. */
  readonly onFillInventorySplit?: (initial: InventorySplitFormInitial) => void;
  /** Called with the values one row hands to `wamn-wms:pallet/create@1.0.0`. */
  readonly onFillPalletCreate?: (initial: PalletCreateFormInitial) => void;
  /** Called with every outcome this screen reads. */
  readonly onOutcome?: (outcome: Outcome<LocationQueryResult>) => void;
}

/** What an operator calls this screen. The page decides where it goes. */
export const LocationQueryTableLabel = "query";

/**
 * The table for `wamn-wms:location/query@1.0.0`: the DataTable over `LOCATION_QUERY_TABLE`.
 *
 * It loads when it mounts. A change to a filter, a sort of rows the load did
 * not read in full, a cap change and a refresh each start a new load.
 */
export function LocationQueryTable(props: LocationQueryTableProps) {
  const [scope, setScope] = createSignal<Partial<LocationQueryRequest>>({});
  const load = createTableLoad<LocationQueryRow>(LOCATION_QUERY_TABLE, async (limit) => {
    let request = { ...scope(), ...props.fixed } as LocationQueryRequest;
    request = writeMember(request, ["limit"], limit) as LocationQueryRequest;
    const outcome = await query(props.transport, [request]);
    props.onOutcome?.(outcome);
    if (outcome.status !== "completed") {
      announceOutcome(outcome, LocationQueryTableLabel);
    }
    return outcome;
  });
  void load.load();
  onCleanup(afterWrites(props.transport, () => void load.load()));

  const changeScope = (filters: readonly DataTableScopeFilter[]) => {
    let next: Partial<LocationQueryRequest> = {};
    for (const filter of filters) {
      switch (filter.field) {
        case "locationCode":
          next = writeMember(next, ["filter", "locationCode"], [...filter.values]);
          break;
      }
    }
    setScope(next);
    void load.load();
  };

  const actions = (row: LocationQueryRow) => (
    <>
      <Show when={props.onOpenLocationGet}>
        <Button type="button" variant="outline" size="sm" onClick={() => props.onOpenLocationGet?.(row)}>
          get
        </Button>
      </Show>
      <Show when={props.onFillInventoryMove}>
        <Button
          type="button"
          variant="outline"
          size="sm"
          onClick={() => props.onFillInventoryMove?.(writeMember({} as InventoryMoveFormInitial, ["value", "toLocationId"], row.id))}
        >
          move
        </Button>
      </Show>
      <Show when={props.onFillInventorySplit}>
        <Button
          type="button"
          variant="outline"
          size="sm"
          onClick={() => props.onFillInventorySplit?.(writeMember({} as InventorySplitFormInitial, ["value", "toLocationId"], row.id))}
        >
          split
        </Button>
      </Show>
      <Show when={props.onFillPalletCreate}>
        <Button
          type="button"
          variant="outline"
          size="sm"
          onClick={() => props.onFillPalletCreate?.(writeMember({} as PalletCreateFormInitial, ["locationId"], row.id))}
        >
          create
        </Button>
      </Show>
    </>
  );

  return (
    <TableScreen>
      <DataTable
        name="location"
        columns={LOCATION_QUERY_TABLE.columns}
        rowId={LOCATION_QUERY_TABLE.rowId}
        rows={load.state().rows}
        fullyRead={load.state().fullyRead}
        busy={load.state().busy}
        refusal={load.state().refusal}
        cap={load.state().cap}
        onCapChange={(cap) => void load.load(cap)}
        onRefresh={() => void load.load()}
        startedAt={load.state().startedAt}
        endedAt={load.state().endedAt}
        sortFields={LOCATION_QUERY_TABLE.sortFields}
        sortMaxFields={LOCATION_QUERY_TABLE.sortMaxFields}
        onSortChange={load.sortBy}
        scopeFilters={LOCATION_QUERY_TABLE.scopeFilters}
        onScopeChange={changeScope}
        rowActions={actions}
      />
    </TableScreen>
  );
}

/** The table definition of `wamn-wms:location/query@1.0.0`. */
export const LOCATION_QUERY_TABLE = {
  read: "query",
  rowId: ["id"],
  pageMaximum: 100,
  scopeFilters: ["locationCode"],
  sortFields: [],
  sortDirections: [],
  sortMaxFields: 1,
  columns: [
    { field: "createdAt", label: "created at", type: "timestamptz", role: "value" },
    { field: "id", label: "id", type: "uuid", role: "key" },
    { field: "locationCode", label: "location code", type: "text", role: "value" },
    { field: "rowVersion", label: "row version", type: "int32", role: "revision" },
  ],
  update: { operation: "wamn-wms:location/update@1.0.0", keyInput: ["id"], revisionInput: ["expectedRowVersion"], revisionField: "rowVersion", fields: [
    { field: "locationCode", input: ["change", "locationCode"] },
  ] },
  actions: [
    { operation: "wamn-wms:location/get@1.0.0", label: "get", many: false },
    { operation: "wamn-wms:inventory/move@1.0.0", label: "move", many: true },
    { operation: "wamn-wms:inventory/split@1.0.0", label: "split", many: true },
    { operation: "wamn-wms:pallet/create@1.0.0", label: "create", many: false },
  ],
  childTables: [
    { definition: "PALLET_QUERY_TABLE", scopeFilter: "locationId" },
  ],
} as const;

/** What an operator types for `wamn-wms:location/update@1.0.0`. */
const UPDATE_INPUT = z.object({
  change: z
    .object({
      locationCode: z.string().optional(),
    })
    .optional(),
});

/** What the form for `wamn-wms:location/update@1.0.0` can start with. */
export interface LocationUpdateFormInitial {
  change?: {
    locationCode?: string;
  };
}

/** What the form for `wamn-wms:location/update@1.0.0` takes. */
export interface LocationUpdateFormProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** Values the form starts with. */
  readonly initial?: LocationUpdateFormInitial;
  /** The record this command changes. The form reads it when it opens, and
   * sends the revision it read, because `wamn-wms:location/get@1.0.0` states that binding. */
  readonly key: LocationGetDetailInput;
  /** Called with the outcome of every submission. */
  readonly onSubmitted?: (outcome: Outcome<LocationUpdateResult>) => void;
}

/** What an operator calls this screen. The page decides where it goes. */
export const LocationUpdateFormLabel = "update";

/**
 * The form for `wamn-wms:location/update@1.0.0`.
 *
 * It renders what the operator fills and nothing else. The reserved inputs
 * come from the runtime at submit time, and the operator never sees them.
 */
export function LocationUpdateForm(props: LocationUpdateFormProps) {
  const [refusal, setRefusal] = createSignal<{ text: string; member: string | null } | null>(null);
  const [done, setDone] = createSignal(false);
  // The record this command changes, read when the form opens and again
  // when its key changes. The form sends the revision of this read, so a
  // change another writer makes after it refuses as a conflict.
  const [record, { refetch: readAgain }] = createResource(
    () => props.key,
    (key: LocationGetDetailInput) => get(props.transport, [key]),
  );

  const form = createForm(() => ({
    defaultValues: { ...props.initial } as Partial<LocationUpdateRequest>,
    onSubmit: async ({ value }: { value: Partial<LocationUpdateRequest> }) => {
      setDone(false);
      const checked = UPDATE_INPUT.safeParse(value);
      if (!checked.success) {
        const issue = checked.error.issues[0];
        setRefusal({
          text: issue?.message ?? "A value is not valid.",
          member: checkedMember(
            issue?.path as (string | number)[] | undefined,
            LOCATION_UPDATE_REQUEST_FIELDS,
          ),
        });
        return;
      }
      let item = { ...value } as LocationUpdateRequest;
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
      announceOutcome(outcome, LocationUpdateFormLabel);
      setRefusal(
        outcome.status === "refused"
          ? { text: refusalSentence(outcome.code, outcome.text), member: refusedMember(outcome.detail) }
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
        <form.Field name={`change.locationCode`}>
          {(field) => (
            <TextField
              label="location code"
              type="text"
              value={String(field().state.value ?? "")}
              onInput={(value) => field().handleChange(value)}
              error={refusalMarks(refusal()?.member ?? null, "change.location_code") ? (refusal()?.text ?? null) : null}
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
