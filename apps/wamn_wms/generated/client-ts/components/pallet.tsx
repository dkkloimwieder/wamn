// @generated from the client-contract IR; do not edit.
//
// `pallet` components. Each one calls the bindings and the runtime, and
// nothing else.

import { Show, createResource, createSignal } from "solid-js";
import { createTable, type ColumnDef } from "@tanstack/solid-table";
import { createForm } from "@tanstack/solid-form";
import { z } from "zod";
import {
  appendPage,
  cellText,
  checkedMember,
  completePair,
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
  ChoiceField,
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
          ? { text: refusalSentence(outcome.code), member: refusedMember(outcome.detail) }
          : null,
      );
      setDone(outcome.status === "completed");
    },
  }));
  const [locationIdOptions, setLocationIdOptions] = createSignal<PageState<LocationQueryRow>>(emptyPage<LocationQueryRow>());
  const [locationIdSearch, setLocationIdSearch] = createSignal("");
  const readLocationIdOptions = async (cursor: string | null) => {
    let request = { requestId: newRequestId() } as LocationQueryRequest;
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
  void readLocationIdOptions(null);

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
  const [outcome] = createResource(
    () => props.input,
    async (input: PalletGetDetailInput) => {
      const read = await get(props.transport, [
        { ...input, requestId: newRequestId() } as PalletGetRequest,
      ]);
      props.onOutcome?.(read);
      if (read.status !== "completed") {
        announceOutcome(read, PalletGetDetailLabel);
      }
      return read;
    },
  );
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
  /** Called when the operator picks one row. */
  readonly onRowSelect?: (row: PalletQueryRow) => void;
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
 * The table for `wamn-wms:pallet/query@1.0.0`.
 *
 * It owns its page controls and its rows. A change to a control clears the
 * rows, because a cursor names a position in the list the old input produced.
 *
 * It reads when the operator asks, and not when it mounts, because a read is
 * a request that the operator did not send yet.
 */
export function PalletQueryTable(props: PalletQueryTableProps) {
  const [controls, setControls] = createSignal<Partial<PalletQueryRequest>>({});
  const [page, setPage] = createSignal<PageState<PalletQueryRow>>(emptyPage<PalletQueryRow>());

  const read = async (cursor: string | null) => {
    setPage(startRead(page()));
    const request = {
      ...completePair(controls(), ["sort", "field"], ["sort", "direction"]),
      ...props.fixed,
      requestId: newRequestId(),
    } as PalletQueryRequest;
    const sent = cursor === null ? request : (writeMember(request, ["cursor"], cursor) as PalletQueryRequest);
    const outcome = await query(props.transport, [sent]);
    props.onOutcome?.(outcome);
    if (outcome.status !== "completed") {
      announceOutcome(outcome, PalletQueryTableLabel);
      setPage(failedRead(page(), outcome));
      return;
    }
    const rows = outcome.value.item;
    setPage(cursor === null ? firstPage(rows, outcome.value.nextCursor) : appendPage(page(), rows, outcome.value.nextCursor));
  };

  const restart = () => {
    setPage(emptyPage<PalletQueryRow>());
    void read(null);
  };

  const change = (path: readonly string[], value: JsonValue) => {
    setControls((current) => writeControl(current, path, value));
    restart();
  };

  const locationGetLabels = createRecordLabels(async (key) => {
    const request = writeMember({ requestId: newRequestId() }, ["id"], key) as LocationGetRequest;
    const outcome = await locationGet(props.transport, [request]);
    if (outcome.status !== "completed") {
      return null;
    }
    const text = outcome.value.locationCode;
    return text == null ? null : String(text);
  });

  const columns: ColumnDef<GridFeatures, PalletQueryRow>[] = [
    {
      accessorKey: "createdAt",
      header: "created at",
      cell: (cell) => cellText(cell.getValue() as JsonValue, "timestamptz"),
    },
    {
      accessorKey: "createdBy",
      header: "created by",
      cell: (cell) => cellText(cell.getValue() as JsonValue, "uuid"),
    },
    {
      accessorKey: "id",
      header: "id",
      cell: (cell) => cellText(cell.getValue() as JsonValue, "uuid"),
    },
    {
      accessorKey: "locationId",
      header: "location id",
      cell: (cell) => <>{locationGetLabels(cell.getValue() as string | null)}</>,
    },
    {
      accessorKey: "palletCode",
      header: "pallet code",
      cell: (cell) => cellText(cell.getValue() as JsonValue, "text"),
    },
    {
      accessorKey: "rowVersion",
      header: "row version",
      cell: (cell) => cellText(cell.getValue() as JsonValue, "int32"),
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
    {
      accessorKey: "updatedAt",
      header: "updated at",
      cell: (cell) => cellText(cell.getValue() as JsonValue, "timestamptz"),
    },
    {
      accessorKey: "updatedBy",
      header: "updated by",
      cell: (cell) => cellText(cell.getValue() as JsonValue, "uuid"),
    },
    {
      id: "openPalletGet",
      header: "",
      cell: (cell) => (
        <Show when={props.onOpenPalletGet}>
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => props.onOpenPalletGet?.(cell.row.original)}
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
            onClick={() => props.onFillInventoryAdjust?.(writeMember({} as InventoryAdjustFormInitial, ["value", "palletId"], cell.row.original.id))}
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
            onClick={() => props.onFillInventoryMove?.(writeMember({} as InventoryMoveFormInitial, ["value", "palletId"], cell.row.original.id))}
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
            onClick={() => props.onFillInventorySplit?.(writeMember({} as InventorySplitFormInitial, ["value", "sourcePalletId"], cell.row.original.id))}
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
      return page().rows as PalletQueryRow[];
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
            label="location id"
            type="text"
            onChange={(value) => change(["filter", "locationId"], value.split(",").filter((part) => part !== ""))}
          />
          <TextField
            label="pallet code"
            type="text"
            onChange={(value) => change(["filter", "palletCode"], value.split(",").filter((part) => part !== ""))}
          />
          <TextField
            label="status"
            type="text"
            onChange={(value) => change(["filter", "status"], value.split(",").filter((part) => part !== ""))}
          />
          <ChoiceField
            label="field"
            allowEmpty={true}
            choices={[
              { value: "created_at", text: "created at" },
              { value: "location_id", text: "location id" },
              { value: "pallet_code", text: "pallet code" },
              { value: "updated_at", text: "updated at" },
            ]}
            onChange={(value) => change(["sort", "field"], value)}
          />
          <ChoiceField
            label="direction"
            allowEmpty={true}
            choices={[
              { value: "ascending", text: "ascending" },
              { value: "descending", text: "descending" },
            ]}
            onChange={(value) => change(["sort", "direction"], value)}
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
