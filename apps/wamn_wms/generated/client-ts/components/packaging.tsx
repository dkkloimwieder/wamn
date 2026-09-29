// @generated from the client-contract IR; do not edit.
//
// `packaging` components. Each one calls the bindings and the runtime, and
// nothing else.

import { Show, createResource, createSignal, onCleanup } from "solid-js";
import * as z from "zod/mini";
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
  type JsonValue,
  type Outcome,
  type PageState,
  type Transport,
  type Uuid,
  writeMember,
} from "@wamn/web-runtime";
import {
  Button,
  ChoiceField,
  DetailItem,
  DetailList,
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
  PACKAGING_CREATE_REQUEST_FIELDS,
  PACKAGING_QUERY_REQUEST_FIELDS,
  PACKAGING_QUERY_RESULT_FIELDS,
  PACKAGING_QUERY_ROUTE,
  create,
  get,
  type PackagingCreateRequest,
  type PackagingCreateResult,
  type PackagingGetRequest,
  type PackagingGetResult,
  type PackagingQueryRequest,
  type PackagingQueryResult,
  type PackagingQueryRow,
} from "../packaging.js";
import {
  PackagingCreateFormLabel,
  PackagingGetDetailLabel,
  PackagingQueryTableLabel,
} from "./labels.js";
import {
  LOCATION_GET_REQUEST_FIELDS,
  LOCATION_GET_RESULT_FIELDS,
  LOCATION_GET_ROUTE,
  get as locationGet,
  query as locationQuery,
  type LocationGetRequest,
  type LocationQueryRequest,
  type LocationQueryRow,
} from "../location.js";

/** What the release accepts: one UUID, hyphenated. */
const UUID_TEXT = /^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$/;

/** What an operator types for `wamn-wms:packaging/create@1.0.0`. */
const CREATE_INPUT = z.object({
  locationId: z.string().check(z.regex(UUID_TEXT, "expected a UUID")),
  packagingCode: z.string(),
  status: z.enum(["available", "held"]),
  type: z.enum(["bin", "case", "loose", "pallet", "tote"]),
});

/** What the form for `wamn-wms:packaging/create@1.0.0` can start with. */
export interface PackagingCreateFormInitial {
  locationId?: Uuid;
  packagingCode?: string;
  status?: "available" | "held";
  type?: "bin" | "case" | "loose" | "pallet" | "tote";
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

/**
 * The form for `wamn-wms:packaging/create@1.0.0`.
 *
 * It renders what the operator fills and nothing else. The reserved inputs
 * come from the runtime at submit time, and the operator never sees them.
 */
export function PackagingCreateForm(props: PackagingCreateFormProps) {
  const [refusal, setRefusal] = createSignal<{ text: string; member: string | null } | null>(null);
  const [done, setDone] = createSignal(false);

  const [locationIdValue, setLocationIdValue] = createSignal<JsonValue | undefined>(
    readMember(props.initial, ["locationId"]) as JsonValue | undefined,
  );
  const [packagingCodeValue, setPackagingCodeValue] = createSignal<JsonValue | undefined>(
    readMember(props.initial, ["packagingCode"]) as JsonValue | undefined,
  );
  const [statusValue, setStatusValue] = createSignal<JsonValue | undefined>(
    readMember(props.initial, ["status"]) as JsonValue | undefined,
  );
  const [typeValue, setTypeValue] = createSignal<JsonValue | undefined>(
    readMember(props.initial, ["type"]) as JsonValue | undefined,
  );

  const submit = async () => {
    setDone(false);
    let value = { ...props.initial } as Partial<PackagingCreateRequest>;
    const hold = (path: readonly string[], member: JsonValue | undefined) => {
      if (member !== undefined) {
        value = writeMember(value, path, member);
      }
    };
    hold(["locationId"], locationIdValue());
    hold(["packagingCode"], packagingCodeValue());
    hold(["status"], statusValue());
    hold(["type"], typeValue());
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
    item = writeMember(item, ["idempotencyKey"], newIdempotencyKey());
    item = writeMember(item, ["requestId"], newRequestId());
    const outcome = await create(props.transport, [item]);
    props.onSubmitted?.(outcome);
    announceOutcome(outcome, PackagingCreateFormLabel);
    setRefusal(
      outcome.status === "refused"
        ? { text: refusalSentence(outcome.code, outcome.text), member: refusedMember(outcome.detail) }
        : null,
    );
    setDone(outcome.status === "completed");
  };
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
        void submit();
      }}
    >
      <Show when={refusal()?.member === null ? refusal() : undefined}>
        <FieldError>{refusal()?.text}</FieldError>
      </Show>
      <FieldGroup>
        <RecordSelect
          label="location id"
          options={locationIdOptions().rows}
          optionValue={(row) => String(row.id)}
          optionLabel={(row) => String(row.locationCode)}
          value={locationIdValue() == null ? null : String(locationIdValue())}
          onChange={(value) => setLocationIdValue(value ?? "")}
          onSearch={(text) => {
            setLocationIdSearch(text);
            void readLocationIdOptions(null);
          }}
          hasNextPage={hasNextPage(locationIdOptions())}
          onNextPage={() => void readLocationIdOptions(locationIdOptions().cursor)}
          readRow={readLocationIdRecord}
          error={refusalMarks(refusal()?.member ?? null, "location_id") ? (refusal()?.text ?? null) : null}
        />
        <TextField
          label="packaging code"
          type="text"
          value={String(packagingCodeValue() ?? "")}
          onInput={(value) => setPackagingCodeValue(value)}
          error={refusalMarks(refusal()?.member ?? null, "packaging_code") ? (refusal()?.text ?? null) : null}
        />
        <ChoiceField
          label="status"
          allowEmpty={false}
          choices={[
            { value: "available", text: "available" },
            { value: "held", text: "held" },
          ]}
          value={String(statusValue() ?? "")}
          onChange={(value) => setStatusValue(value as "available" | "held")}
          error={refusalMarks(refusal()?.member ?? null, "status") ? (refusal()?.text ?? null) : null}
        />
        <ChoiceField
          label="type"
          allowEmpty={false}
          choices={[
            { value: "bin", text: "bin" },
            { value: "case", text: "case" },
            { value: "loose", text: "loose" },
            { value: "pallet", text: "pallet" },
            { value: "tote", text: "tote" },
          ]}
          value={String(typeValue() ?? "")}
          onChange={(value) => setTypeValue(value as "bin" | "case" | "loose" | "pallet" | "tote")}
          error={refusalMarks(refusal()?.member ?? null, "type") ? (refusal()?.text ?? null) : null}
        />
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

/**
 * The detail screen for `wamn-wms:packaging/get@1.0.0`.
 *
 * It reads when it mounts and again whenever its input changes, because the
 * input names the record it shows.
 */
export function PackagingGetDetail(props: PackagingGetDetailProps) {
  const [outcome, { refetch: readAgain }] = createResource(
    () => props.input,
    async (input: PackagingGetDetailInput) => {
      const read = await get(props.transport, [
        input as PackagingGetRequest,
      ]);
      props.onOutcome?.(read);
      if (read.status !== "completed") {
        announceOutcome(read, PackagingGetDetailLabel);
      }
      return read;
    },
  );
  onCleanup(afterWrites(props.transport, () => void readAgain()));
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
        <DetailItem term="created at">{cellText(readMember(record(), ["createdAt"]), "timestamptz")}</DetailItem>
        <DetailItem term="created by">{cellText(readMember(record(), ["createdBy"]), "uuid")}</DetailItem>
        <DetailItem term="id">{cellText(readMember(record(), ["id"]), "uuid")}</DetailItem>
        <DetailItem term="located at">{cellText(readMember(record(), ["locatedAt"]), "timestamptz")}</DetailItem>
        <DetailItem term="location id">{cellText(readMember(record(), ["locationId"]), "uuid")}</DetailItem>
        <DetailItem term="packaging code">{cellText(readMember(record(), ["packagingCode"]), "text")}</DetailItem>
        <DetailItem term="row version">{cellText(readMember(record(), ["rowVersion"]), "int32")}</DetailItem>
        <DetailItem term="status">{cellText(readMember(record(), ["status"]), "text")}</DetailItem>
        <DetailItem term="type">{cellText(readMember(record(), ["type"]), "text")}</DetailItem>
        <DetailItem term="updated at">{cellText(readMember(record(), ["updatedAt"]), "timestamptz")}</DetailItem>
        <DetailItem term="updated by">{cellText(readMember(record(), ["updatedBy"]), "uuid")}</DetailItem>
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
  /** For each operation whose record a row opens, what opening it does. A row shows a button only for these. */
  readonly onOpen?: { readonly [operation: string]: (row: PackagingQueryRow) => void };
  /** For each operation whose form a row fills, what the filled values do. */
  readonly onFill?: { readonly [operation: string]: (initial: object) => void };
  /** Called with every outcome this screen reads. */
  readonly onOutcome?: (outcome: Outcome<PackagingQueryResult>) => void;
}

/** The table for `wamn-wms:packaging/query@1.0.0`: the QueryTable over `PACKAGING_QUERY_TABLE`, in the table screen. */
export function PackagingQueryTable(props: PackagingQueryTableProps) {
  return (
    <TableScreen>
      <QueryTable<PackagingQueryRow, PackagingQueryResult> definition={PACKAGING_QUERY_TABLE} label={PackagingQueryTableLabel} {...props} />
    </TableScreen>
  );
}

/** The table definition of `wamn-wms:packaging/query@1.0.0`. */
export const PACKAGING_QUERY_TABLE = {
  name: "packaging",
  read: { route: PACKAGING_QUERY_ROUTE, request: PACKAGING_QUERY_REQUEST_FIELDS, result: PACKAGING_QUERY_RESULT_FIELDS },
  rows: "item",
  rowId: ["id"],
  pageMaximum: 100,
  limitInput: ["limit"],
  sortFieldInput: ["sort", "field"],
  sortDirectionInput: ["sort", "direction"],
  filters: [{ field: "locationId", input: ["filter", "locationId"], list: true }, { field: "packagingCode", input: ["filter", "packagingCode"], list: true, match: "contains" }, { field: "status", input: ["filter", "status"], list: true }],
  scopeFilters: ["locationId", "packagingCode", "status"],
  sortFields: [{ field: "createdAt", wire: "created_at" }, { field: "locationId", wire: "location_id" }, { field: "packagingCode", wire: "packaging_code" }, { field: "updatedAt", wire: "updated_at" }],
  sortDirections: ["ascending", "descending"],
  sortMaxFields: 1,
  columns: [
    { field: "createdAt", label: "created at", type: "timestamptz", role: "value" },
    { field: "createdBy", label: "created by", type: "uuid", role: "value" },
    { field: "id", label: "id", type: "uuid", role: "key" },
    { field: "locatedAt", label: "located at", type: "timestamptz", role: "value" },
    { field: "locationId", label: "location id", type: "uuid", role: "reference", displayField: "locationCode", recordRead: { read: { route: LOCATION_GET_ROUTE, request: LOCATION_GET_REQUEST_FIELDS, result: LOCATION_GET_RESULT_FIELDS }, keyInput: ["id"] } },
    { field: "packagingCode", label: "packaging code", type: "text", role: "value" },
    { field: "rowVersion", label: "row version", type: "int32", role: "revision" },
    { field: "status", label: "status", type: "text", role: "value" },
    { field: "type", label: "type", type: "text", role: "value" },
    { field: "updatedAt", label: "updated at", type: "timestamptz", role: "value" },
    { field: "updatedBy", label: "updated by", type: "uuid", role: "value" },
  ],
  actions: [
    { operation: "wamn-wms:packaging/get@1.0.0", label: "get", many: false, opens: "record", fill: [] },
    { operation: "wamn-wms:inventory/adjust@1.0.0", label: "adjust", many: true, opens: "form", fill: [{ field: "id", input: ["value", "packagingId"] }], revision: { field: "rowVersion", input: ["value", "expectedRowVersion"] }, form: () => import("./inventory.js").then((module) => ({ default: module.InventoryAdjustForm })) },
    { operation: "wamn-wms:inventory/move@1.0.0", label: "move", many: true, opens: "form", fill: [{ field: "id", input: ["value", "packagingId"] }], revision: { field: "rowVersion", input: ["value", "expectedRowVersion"] }, form: () => import("./inventory.js").then((module) => ({ default: module.InventoryMoveForm })) },
    { operation: "wamn-wms:inventory/split@1.0.0", label: "split", many: true, opens: "form", fill: [{ field: "id", input: ["value", "sourcePackagingId"] }], revision: { field: "rowVersion", input: ["value", "expectedRowVersion"] }, form: () => import("./inventory.js").then((module) => ({ default: module.InventorySplitForm })) },
  ],
  childTables: [],
} as const;
