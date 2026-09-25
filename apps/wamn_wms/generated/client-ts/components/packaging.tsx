// @generated from the client-contract IR; do not edit.
//
// `packaging` components. Each one calls the bindings and the runtime, and
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
  PACKAGING_CLOSE_REQUEST_FIELDS,
  PACKAGING_CREATE_REQUEST_FIELDS,
  close,
  create,
  get,
  get as packagingGet,
  query,
  query as packagingQuery,
  type PackagingCloseRequest,
  type PackagingCloseResult,
  type PackagingCreateRequest,
  type PackagingCreateResult,
  type PackagingGetRequest,
  type PackagingGetResult,
  type PackagingQueryRequest,
  type PackagingQueryResult,
  type PackagingQueryRow,
} from "../packaging.js";
import {
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

/** What an operator types for `wamn-wms:packaging/close@1.0.0`. */
const CLOSE_INPUT = z.object({
  value: z
    .object({
      packagingId: z.string().regex(UUID_TEXT, "expected a UUID"),
    })
    .optional(),
});

/** What the form for `wamn-wms:packaging/close@1.0.0` can start with. */
export interface PackagingCloseFormInitial {
  value?: {
    packagingId?: Uuid;
  };
}

/** What the form for `wamn-wms:packaging/close@1.0.0` takes. */
export interface PackagingCloseFormProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** Values the form starts with. */
  readonly initial?: PackagingCloseFormInitial;
  /** The revision this command sends. The release binds no read that supplies it. */
  readonly valueExpectedRowVersion: PackagingCloseRequest["value"]["expectedRowVersion"];
  /** Called with the outcome of every submission. */
  readonly onSubmitted?: (outcome: Outcome<PackagingCloseResult>) => void;
}

/** What an operator calls this screen. The page decides where it goes. */
export const PackagingCloseFormLabel = "close";

/**
 * The form for `wamn-wms:packaging/close@1.0.0`.
 *
 * It renders what the operator fills and nothing else. The reserved inputs
 * come from the runtime at submit time, and the operator never sees them.
 */
export function PackagingCloseForm(props: PackagingCloseFormProps) {
  const [refusal, setRefusal] = createSignal<{ text: string; member: string | null } | null>(null);
  const [done, setDone] = createSignal(false);

  const form = createForm(() => ({
    defaultValues: { ...props.initial } as Partial<PackagingCloseRequest>,
    onSubmit: async ({ value }: { value: Partial<PackagingCloseRequest> }) => {
      setDone(false);
      const checked = CLOSE_INPUT.safeParse(value);
      if (!checked.success) {
        const issue = checked.error.issues[0];
        setRefusal({
          text: issue?.message ?? "A value is not valid.",
          member: checkedMember(
            issue?.path as (string | number)[] | undefined,
            PACKAGING_CLOSE_REQUEST_FIELDS,
          ),
        });
        return;
      }
      let item = { ...value } as PackagingCloseRequest;
      item = writeMember(item, ["requestId"], newRequestId());
      item = writeMember(item, ["value", "idempotencyKey"], newIdempotencyKey());
      item = writeMember(item, ["value", "expectedRowVersion"], props.valueExpectedRowVersion);
      const outcome = await close(props.transport, [item]);
      props.onSubmitted?.(outcome);
      announceOutcome(outcome, PackagingCloseFormLabel);
      setRefusal(
        outcome.status === "refused"
          ? { text: refusalSentence(outcome.code), member: refusedMember(outcome.detail) }
          : null,
      );
      setDone(outcome.status === "completed");
    },
  }));
  const [valuePackagingIdOptions, setValuePackagingIdOptions] = createSignal<PageState<PackagingQueryRow>>(emptyPage<PackagingQueryRow>());
  const readValuePackagingIdOptions = async (cursor: string | null) => {
    let request = {} as PackagingQueryRequest;
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
        <form.Field name={`value.packagingId`}>
          {(field) => (
            <RecordSelect
              label="packaging id"
              options={valuePackagingIdOptions().rows}
              optionValue={(row) => String(row.id)}
              optionLabel={(row) => String(row.code)}
              value={field().state.value == null ? null : String(field().state.value)}
              onChange={(value) => field().handleChange(value ?? "")}
              hasNextPage={hasNextPage(valuePackagingIdOptions())}
              onNextPage={() => void readValuePackagingIdOptions(valuePackagingIdOptions().cursor)}
              readRow={readValuePackagingIdRecord}
              error={refusalMarks(refusal()?.member ?? null, "value.packaging_id") ? (refusal()?.text ?? null) : null}
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

/** What an operator types for `wamn-wms:packaging/create@1.0.0`. */
const CREATE_INPUT = z.object({
  value: z
    .object({
      code: z.string(),
      locationId: z.string().regex(UUID_TEXT, "expected a UUID"),
      type: z.string(),
    })
    .optional(),
});

/** What the form for `wamn-wms:packaging/create@1.0.0` can start with. */
export interface PackagingCreateFormInitial {
  value?: {
    code?: string;
    locationId?: Uuid;
    type?: string;
  };
}

/** What the form for `wamn-wms:packaging/create@1.0.0` takes. */
export interface PackagingCreateFormProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** Values the form starts with. */
  readonly initial?: PackagingCreateFormInitial;
  /** Called with the outcome of every submission. */
  readonly onSubmitted?: (outcome: Outcome<PackagingCreateResult>) => void;
}

/** What an operator calls this screen. The page decides where it goes. */
export const PackagingCreateFormLabel = "create";

/**
 * The form for `wamn-wms:packaging/create@1.0.0`.
 *
 * It renders what the operator fills and nothing else. The reserved inputs
 * come from the runtime at submit time, and the operator never sees them.
 */
export function PackagingCreateForm(props: PackagingCreateFormProps) {
  const [refusal, setRefusal] = createSignal<{ text: string; member: string | null } | null>(null);
  const [done, setDone] = createSignal(false);

  const form = createForm(() => ({
    defaultValues: { ...props.initial } as Partial<PackagingCreateRequest>,
    onSubmit: async ({ value }: { value: Partial<PackagingCreateRequest> }) => {
      setDone(false);
      const checked = CREATE_INPUT.safeParse(value);
      if (!checked.success) {
        const issue = checked.error.issues[0];
        setRefusal({
          text: issue?.message ?? "A value is not valid.",
          member: checkedMember(
            issue?.path as (string | number)[] | undefined,
            PACKAGING_CREATE_REQUEST_FIELDS,
          ),
        });
        return;
      }
      let item = { ...value } as PackagingCreateRequest;
      item = writeMember(item, ["requestId"], newRequestId());
      item = writeMember(item, ["value", "idempotencyKey"], newIdempotencyKey());
      const outcome = await create(props.transport, [item]);
      props.onSubmitted?.(outcome);
      announceOutcome(outcome, PackagingCreateFormLabel);
      setRefusal(
        outcome.status === "refused"
          ? { text: refusalSentence(outcome.code), member: refusedMember(outcome.detail) }
          : null,
      );
      setDone(outcome.status === "completed");
    },
  }));
  const [valueLocationIdOptions, setValueLocationIdOptions] = createSignal<PageState<LocationQueryRow>>(emptyPage<LocationQueryRow>());
  const [valueLocationIdSearch, setValueLocationIdSearch] = createSignal("");
  const readValueLocationIdOptions = async (cursor: string | null) => {
    let request = {} as LocationQueryRequest;
    if (valueLocationIdSearch() !== "") {
      request = writeMember(request, ["filter", "locationCode"], [valueLocationIdSearch()]) as LocationQueryRequest;
    }
    if (cursor !== null) {
      request = writeMember(request, ["cursor"], cursor) as LocationQueryRequest;
    }
    const outcome = await locationQuery(props.transport, [request]);
    if (outcome.status !== "completed") {
      return;
    }
    const rows = outcome.value.item as LocationQueryRow[];
    setValueLocationIdOptions(
      cursor === null
        ? firstPage(rows, outcome.value.nextCursor)
        : appendPage(valueLocationIdOptions(), rows, outcome.value.nextCursor),
    );
  };
  const readValueLocationIdRecord = async (key: string): Promise<LocationQueryRow | null> => {
    const request = writeMember({}, ["id"], key) as LocationGetRequest;
    const outcome = await locationGet(props.transport, [request]);
    return outcome.status === "completed" ? ((outcome.value ?? null) as unknown as LocationQueryRow | null) : null;
  };
  void readValueLocationIdOptions(null);

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
        <form.Field name={`value.code`}>
          {(field) => (
            <TextField
              label="code"
              type="text"
              value={String(field().state.value ?? "")}
              onInput={(value) => field().handleChange(value)}
              error={refusalMarks(refusal()?.member ?? null, "value.code") ? (refusal()?.text ?? null) : null}
            />
          )}
        </form.Field>
        <form.Field name={`value.locationId`}>
          {(field) => (
            <RecordSelect
              label="location id"
              options={valueLocationIdOptions().rows}
              optionValue={(row) => String(row.id)}
              optionLabel={(row) => String(row.locationCode)}
              value={field().state.value == null ? null : String(field().state.value)}
              onChange={(value) => field().handleChange(value ?? "")}
              onSearch={(text) => {
                setValueLocationIdSearch(text);
                void readValueLocationIdOptions(null);
              }}
              hasNextPage={hasNextPage(valueLocationIdOptions())}
              onNextPage={() => void readValueLocationIdOptions(valueLocationIdOptions().cursor)}
              readRow={readValueLocationIdRecord}
              error={refusalMarks(refusal()?.member ?? null, "value.location_id") ? (refusal()?.text ?? null) : null}
            />
          )}
        </form.Field>
        <form.Field name={`value.type`}>
          {(field) => (
            <TextField
              label="type"
              type="text"
              value={String(field().state.value ?? "")}
              onInput={(value) => field().handleChange(value)}
              error={refusalMarks(refusal()?.member ?? null, "value.type") ? (refusal()?.text ?? null) : null}
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

/** The record that the detail for `wamn-wms:packaging/get@1.0.0` reads. */
export interface PackagingGetDetailInput {
  readonly id: Uuid;
}

/** What the detail screen for `wamn-wms:packaging/get@1.0.0` takes. */
export interface PackagingGetDetailProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** The input that names the record. */
  readonly input: PackagingGetDetailInput;
  /** Called with every outcome this screen reads. */
  readonly onOutcome?: (outcome: Outcome<PackagingGetResult>) => void;
}

/** What an operator calls this screen. The page decides where it goes. */
export const PackagingGetDetailLabel = "get";

/**
 * The detail screen for `wamn-wms:packaging/get@1.0.0`.
 *
 * It reads when it mounts and again whenever its input changes, because the
 * input names the record it shows.
 */
export function PackagingGetDetail(props: PackagingGetDetailProps) {
  const [outcome] = createResource(
    () => props.input,
    async (input: PackagingGetDetailInput) => {
      const read = await get(props.transport, [
        { ...input } as PackagingGetRequest,
      ]);
      props.onOutcome?.(read);
      if (read.status !== "completed") {
        announceOutcome(read, PackagingGetDetailLabel);
      }
      return read;
    },
  );
  const record = (): PackagingGetResult | undefined => {
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
        <DetailItem term="code">{cellText(readMember(record(), ["code"]), "text")}</DetailItem>
        <DetailItem term="created at">{cellText(readMember(record(), ["createdAt"]), "timestamptz")}</DetailItem>
        <DetailItem term="id">{cellText(readMember(record(), ["id"]), "uuid")}</DetailItem>
        <DetailItem term="lifecycle">{cellText(readMember(record(), ["lifecycle"]), "text")}</DetailItem>
        <DetailItem term="location id">{cellText(readMember(record(), ["locationId"]), "uuid")}</DetailItem>
        <DetailItem term="row version">{cellText(readMember(record(), ["rowVersion"]), "int32")}</DetailItem>
        <DetailItem term="type">{cellText(readMember(record(), ["type"]), "text")}</DetailItem>
      </DetailList>
    </section>
  );
}

/** What the table for `wamn-wms:packaging/query@1.0.0` takes. */
export interface PackagingQueryTableProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** Input the parent fixes, which the operator does not edit. */
  readonly fixed?: Partial<PackagingQueryRequest>;
  /** Called when the operator picks one row. */
  readonly onRowSelect?: (row: PackagingQueryRow) => void;
  /** Called when the operator opens `wamn-wms:packaging/get@1.0.0` from one row. */
  readonly onOpenPackagingGet?: (row: PackagingQueryRow) => void;
  /** Called with the values one row hands to `wamn-wms:inventory/move@1.0.0`. */
  readonly onFillInventoryMove?: (initial: InventoryMoveFormInitial) => void;
  /** Called with the values one row hands to `wamn-wms:inventory/split@1.0.0`. */
  readonly onFillInventorySplit?: (initial: InventorySplitFormInitial) => void;
  /** Called with the values one row hands to `wamn-wms:packaging/close@1.0.0`. */
  readonly onFillPackagingClose?: (initial: PackagingCloseFormInitial) => void;
  /** Called with every outcome this screen reads. */
  readonly onOutcome?: (outcome: Outcome<PackagingQueryResult>) => void;
}

/** What an operator calls this screen. The page decides where it goes. */
export const PackagingQueryTableLabel = "query";

/**
 * The table for `wamn-wms:packaging/query@1.0.0`.
 *
 * It owns its page controls and its rows. A change to a control clears the
 * rows, because a cursor names a position in the list the old input produced.
 *
 * It reads when the operator asks, and not when it mounts, because a read is
 * a request that the operator did not send yet.
 */
export function PackagingQueryTable(props: PackagingQueryTableProps) {
  const [controls, setControls] = createSignal<Partial<PackagingQueryRequest>>({});
  const [page, setPage] = createSignal<PageState<PackagingQueryRow>>(emptyPage<PackagingQueryRow>());

  const read = async (cursor: string | null) => {
    setPage(startRead(page()));
    const request = {
      ...controls(),
      ...props.fixed,
    } as PackagingQueryRequest;
    const sent = cursor === null ? request : (writeMember(request, ["cursor"], cursor) as PackagingQueryRequest);
    const outcome = await query(props.transport, [sent]);
    props.onOutcome?.(outcome);
    if (outcome.status !== "completed") {
      announceOutcome(outcome, PackagingQueryTableLabel);
      setPage(failedRead(page(), outcome));
      return;
    }
    const rows = outcome.value.item;
    setPage(cursor === null ? firstPage(rows, outcome.value.nextCursor) : appendPage(page(), rows, outcome.value.nextCursor));
  };

  const restart = () => {
    setPage(emptyPage<PackagingQueryRow>());
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

  const columns: ColumnDef<GridFeatures, PackagingQueryRow>[] = [
    {
      accessorKey: "code",
      header: "code",
      cell: (cell) => cellText(cell.getValue() as JsonValue, "text"),
    },
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
      accessorKey: "rowVersion",
      header: "row version",
      cell: (cell) => cellText(cell.getValue() as JsonValue, "int32"),
    },
    {
      accessorKey: "type",
      header: "type",
      cell: (cell) => cellText(cell.getValue() as JsonValue, "text"),
    },
    {
      id: "openPackagingGet",
      header: "",
      cell: (cell) => (
        <Show when={props.onOpenPackagingGet}>
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => props.onOpenPackagingGet?.(cell.row.original)}
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
            onClick={() => props.onFillInventoryMove?.(writeMember({} as InventoryMoveFormInitial, ["value", "toPackagingId"], cell.row.original.id))}
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
            onClick={() => props.onFillInventorySplit?.(writeMember({} as InventorySplitFormInitial, ["value", "toPackagingId"], cell.row.original.id))}
          >
            split
          </Button>
        </Show>
      ),
    },
    {
      id: "fillPackagingClose",
      header: "",
      cell: (cell) => (
        <Show when={props.onFillPackagingClose}>
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => props.onFillPackagingClose?.(writeMember({} as PackagingCloseFormInitial, ["value", "packagingId"], cell.row.original.id))}
          >
            close
          </Button>
        </Show>
      ),
    },
  ];

  const table = createTable({
    features: gridFeatures,
    get data() {
      return page().rows as PackagingQueryRow[];
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
