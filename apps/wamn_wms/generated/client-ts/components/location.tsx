// @generated from the client-contract IR; do not edit.
//
// `location` components. Each one calls the bindings and the runtime, and
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
  readMember,
  refusalMarks,
  refusalSentence,
  refusedMember,
  startRead,
  type JsonValue,
  type Outcome,
  type PageState,
  type Transport,
  type Uuid,
  writeControl,
  writeMember,
} from "@wamn/web-runtime";
import {
  Button,
  DataGrid,
  DataGridContainer,
  DetailItem,
  DetailList,
  FieldError,
  FieldGroup,
  FormActions,
  FormDone,
  TableScreen,
  TextField,
  WindowedTable,
  announceOutcome,
  gridFeatures,
  type GridFeatures,
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
  const [outcome] = createResource(
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

/** Columns of `wamn-wms:location/query@1.0.0`, in contract order. */
const QUERY_COLUMNS: ColumnDef<GridFeatures, LocationQueryRow>[] = [
  {
    accessorKey: "createdAt",
    header: "created at",
    cell: (cell) => cellText(cell.getValue() as JsonValue, "timestamptz"),
  },
  {
    accessorKey: "id",
    header: "id",
    cell: (cell) => cellText(cell.getValue() as JsonValue, "uuid"),
  },
  {
    accessorKey: "locationCode",
    header: "location code",
    cell: (cell) => cellText(cell.getValue() as JsonValue, "text"),
  },
  {
    accessorKey: "rowVersion",
    header: "row version",
    cell: (cell) => cellText(cell.getValue() as JsonValue, "int32"),
  },
];

/** What the table for `wamn-wms:location/query@1.0.0` takes. */
export interface LocationQueryTableProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** Input the parent fixes, which the operator does not edit. */
  readonly fixed?: Partial<LocationQueryRequest>;
  /** Called when the operator picks one row. */
  readonly onRowSelect?: (row: LocationQueryRow) => void;
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
 * The table for `wamn-wms:location/query@1.0.0`.
 *
 * It owns its page controls and its rows. A change to a control clears the
 * rows, because a cursor names a position in the list the old input produced.
 *
 * It reads when the operator asks, and not when it mounts, because a read is
 * a request that the operator did not send yet.
 */
export function LocationQueryTable(props: LocationQueryTableProps) {
  const [controls, setControls] = createSignal<Partial<LocationQueryRequest>>({});
  const [page, setPage] = createSignal<PageState<LocationQueryRow>>(emptyPage<LocationQueryRow>());

  const read = async (cursor: string | null) => {
    setPage(startRead(page()));
    const request = {
      ...controls(),
      ...props.fixed,
    } as LocationQueryRequest;
    const sent = cursor === null ? request : (writeMember(request, ["cursor"], cursor) as LocationQueryRequest);
    const outcome = await query(props.transport, [sent]);
    props.onOutcome?.(outcome);
    if (outcome.status !== "completed") {
      announceOutcome(outcome, LocationQueryTableLabel);
      setPage(failedRead(page(), outcome));
      return;
    }
    const rows = outcome.value.item;
    setPage(cursor === null ? firstPage(rows, outcome.value.nextCursor) : appendPage(page(), rows, outcome.value.nextCursor));
  };

  const restart = () => {
    setPage(emptyPage<LocationQueryRow>());
    void read(null);
  };

  const change = (path: readonly string[], value: JsonValue) => {
    setControls((current) => writeControl(current, path, value));
    restart();
  };

  const columns: ColumnDef<GridFeatures, LocationQueryRow>[] = [
    ...QUERY_COLUMNS,
    {
      id: "openLocationGet",
      header: "",
      cell: (cell) => (
        <Show when={props.onOpenLocationGet}>
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => props.onOpenLocationGet?.(cell.row.original)}
          >
            get
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
            onClick={() => props.onFillInventoryMove?.(writeMember({} as InventoryMoveFormInitial, ["value", "toLocationId"], cell.row.original.id))}
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
            onClick={() => props.onFillInventorySplit?.(writeMember({} as InventorySplitFormInitial, ["value", "toLocationId"], cell.row.original.id))}
          >
            split
          </Button>
        </Show>
      ),
    },
    {
      id: "fillPalletCreate",
      header: "",
      cell: (cell) => (
        <Show when={props.onFillPalletCreate}>
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => props.onFillPalletCreate?.(writeMember({} as PalletCreateFormInitial, ["locationId"], cell.row.original.id))}
          >
            create
          </Button>
        </Show>
      ),
    },
  ];

  const table = createTable({
    features: gridFeatures,
    get data() {
      return page().rows as LocationQueryRow[];
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
            label="location code"
            type="text"
            onChange={(value) => change(["filter", "locationCode"], value.split(",").filter((part) => part !== ""))}
          />
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
          <WindowedTable />
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
