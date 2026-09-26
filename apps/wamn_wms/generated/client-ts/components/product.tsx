// @generated from the client-contract IR; do not edit.
//
// `product` components. Each one calls the bindings and the runtime, and
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
  DetailItem,
  DetailList,
  FieldError,
  FieldGroup,
  FormActions,
  FormDone,
  QueryTable,
  TextField,
  announceOutcome,
} from "@wamn/ui";
import {
  PRODUCT_CREATE_REQUEST_FIELDS,
  PRODUCT_QUERY_REQUEST_FIELDS,
  PRODUCT_QUERY_RESULT_FIELDS,
  PRODUCT_QUERY_ROUTE,
  PRODUCT_UPDATE_REQUEST_FIELDS,
  PRODUCT_UPDATE_RESULT_FIELDS,
  PRODUCT_UPDATE_ROUTE,
  create,
  get,
  type ProductCreateRequest,
  type ProductCreateResult,
  type ProductGetRequest,
  type ProductGetResult,
  type ProductQueryRequest,
  type ProductQueryResult,
  type ProductQueryRow,
  type ProductUpdateRequest,
  type ProductUpdateResult,
  update,
} from "../product.js";

/** What an operator types for `wamn-wms:product/create@1.0.0`. */
const CREATE_INPUT = z.object({
  productCode: z.string(),
});

/** What the form for `wamn-wms:product/create@1.0.0` can start with. */
export interface ProductCreateFormInitial {
  productCode?: string;
}

/** What the form for `wamn-wms:product/create@1.0.0` takes. */
export interface ProductCreateFormProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** Values the form starts with. */
  readonly initial?: ProductCreateFormInitial;
  /** Called with the outcome of every submission. */
  readonly onSubmitted?: (outcome: Outcome<ProductCreateResult>) => void;
}

/** What an operator calls this screen. The page decides where it goes. */
export const ProductCreateFormLabel = "create";

/**
 * The form for `wamn-wms:product/create@1.0.0`.
 *
 * It renders what the operator fills and nothing else. The reserved inputs
 * come from the runtime at submit time, and the operator never sees them.
 */
export function ProductCreateForm(props: ProductCreateFormProps) {
  const [refusal, setRefusal] = createSignal<{ text: string; member: string | null } | null>(null);
  const [done, setDone] = createSignal(false);

  const form = createForm(() => ({
    defaultValues: { ...props.initial } as Partial<ProductCreateRequest>,
    onSubmit: async ({ value }: { value: Partial<ProductCreateRequest> }) => {
      setDone(false);
      const checked = CREATE_INPUT.safeParse(value);
      if (!checked.success) {
        const issue = checked.error.issues[0];
        setRefusal({
          text: issue?.message ?? "A value is not valid.",
          member: checkedMember(
            issue?.path as (string | number)[] | undefined,
            PRODUCT_CREATE_REQUEST_FIELDS,
          ),
        });
        return;
      }
      let item = { ...value } as ProductCreateRequest;
      item = writeMember(item, ["idempotencyKey"], newIdempotencyKey());
      item = writeMember(item, ["requestId"], newRequestId());
      const outcome = await create(props.transport, [item]);
      props.onSubmitted?.(outcome);
      announceOutcome(outcome, ProductCreateFormLabel);
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
        <form.Field name={`productCode`}>
          {(field) => (
            <TextField
              label="product code"
              type="text"
              value={String(field().state.value ?? "")}
              onInput={(value) => field().handleChange(value)}
              error={refusalMarks(refusal()?.member ?? null, "product_code") ? (refusal()?.text ?? null) : null}
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

/** The record that the detail for `wamn-wms:product/get@1.0.0` reads. */
export interface ProductGetDetailInput {
  readonly id: Uuid;
}

/** What the detail screen for `wamn-wms:product/get@1.0.0` takes. */
export interface ProductGetDetailProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** The input that names the record. */
  readonly input: ProductGetDetailInput;
  /** Called with every outcome this screen reads. */
  readonly onOutcome?: (outcome: Outcome<ProductGetResult>) => void;
}

/** What an operator calls this screen. The page decides where it goes. */
export const ProductGetDetailLabel = "get";

/**
 * The detail screen for `wamn-wms:product/get@1.0.0`.
 *
 * It reads when it mounts and again whenever its input changes, because the
 * input names the record it shows.
 */
export function ProductGetDetail(props: ProductGetDetailProps) {
  const [outcome, { refetch: readAgain }] = createResource(
    () => props.input,
    async (input: ProductGetDetailInput) => {
      const read = await get(props.transport, [
        input as ProductGetRequest,
      ]);
      props.onOutcome?.(read);
      if (read.status !== "completed") {
        announceOutcome(read, ProductGetDetailLabel);
      }
      return read;
    },
  );
  onCleanup(afterWrites(props.transport, () => void readAgain()));
  const record = (): ProductGetResult | undefined => {
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
        <DetailItem term="product code">{cellText(readMember(record(), ["productCode"]), "text")}</DetailItem>
        <DetailItem term="row version">{cellText(readMember(record(), ["rowVersion"]), "int32")}</DetailItem>
      </DetailList>
    </section>
  );
}

/** What the table for `wamn-wms:product/query@1.0.0` takes. */
export interface ProductQueryTableProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** Input the parent fixes, which the operator does not edit. */
  readonly fixed?: Partial<ProductQueryRequest>;
  /** For each operation whose record a row opens, what opening it does. A row shows a button only for these. */
  readonly onOpen?: { readonly [operation: string]: (row: ProductQueryRow) => void };
  /** For each operation whose form a row fills, what the filled values do. */
  readonly onFill?: { readonly [operation: string]: (initial: object) => void };
  /** Called with every outcome this screen reads. */
  readonly onOutcome?: (outcome: Outcome<ProductQueryResult>) => void;
}

/** What an operator calls this screen. The page decides where it goes. */
export const ProductQueryTableLabel = "query";

/** The table for `wamn-wms:product/query@1.0.0`: the QueryTable over `PRODUCT_QUERY_TABLE`. */
export function ProductQueryTable(props: ProductQueryTableProps) {
  return <QueryTable<ProductQueryRow, ProductQueryResult> definition={PRODUCT_QUERY_TABLE} label={ProductQueryTableLabel} {...props} />;
}

/** The table definition of `wamn-wms:product/query@1.0.0`. */
export const PRODUCT_QUERY_TABLE = {
  name: "product",
  read: { route: PRODUCT_QUERY_ROUTE, request: PRODUCT_QUERY_REQUEST_FIELDS, result: PRODUCT_QUERY_RESULT_FIELDS },
  rows: "item",
  rowId: ["id"],
  pageMaximum: 100,
  limitInput: ["limit"],
  sortFieldInput: null,
  sortDirectionInput: null,
  filters: [{ field: "productCode", input: ["filter", "productCode"], list: true }],
  scopeFilters: ["productCode"],
  sortFields: [],
  sortDirections: [],
  sortMaxFields: 1,
  columns: [
    { field: "createdAt", label: "created at", type: "timestamptz", role: "value" },
    { field: "id", label: "id", type: "uuid", role: "key" },
    { field: "productCode", label: "product code", type: "text", role: "value" },
    { field: "rowVersion", label: "row version", type: "int32", role: "revision" },
  ],
  update: { operation: "wamn-wms:product/update@1.0.0", binding: { route: PRODUCT_UPDATE_ROUTE, request: PRODUCT_UPDATE_REQUEST_FIELDS, result: PRODUCT_UPDATE_RESULT_FIELDS }, keyInput: ["id"], revisionInput: ["expectedRowVersion"], revisionField: "rowVersion", supplied: [{ input: ["requestId"], kind: "requestId" }], fields: [
    { field: "productCode", input: ["change", "productCode"] },
  ] },
  actions: [
    { operation: "wamn-wms:product/get@1.0.0", label: "get", many: false, opens: "record", fill: [] },
    { operation: "wamn-wms:inventory/adjust@1.0.0", label: "adjust", many: true, opens: "form", fill: [{ field: "id", input: ["value", "productId"] }] },
    { operation: "wamn-wms:inventory/split@1.0.0", label: "split", many: true, opens: "form", fill: [{ field: "id", input: ["value", "productId"] }] },
  ],
  childTables: [],
} as const;

/** What an operator types for `wamn-wms:product/update@1.0.0`. */
const UPDATE_INPUT = z.object({
  change: z
    .object({
      productCode: z.string().optional(),
    })
    .optional(),
});

/** What the form for `wamn-wms:product/update@1.0.0` can start with. */
export interface ProductUpdateFormInitial {
  change?: {
    productCode?: string;
  };
}

/** What the form for `wamn-wms:product/update@1.0.0` takes. */
export interface ProductUpdateFormProps {
  /** The transport the application supplies. */
  readonly transport: Transport;
  /** Values the form starts with. */
  readonly initial?: ProductUpdateFormInitial;
  /** The record this command changes. The form reads it when it opens, and
   * sends the revision it read, because `wamn-wms:product/get@1.0.0` states that binding. */
  readonly key: ProductGetDetailInput;
  /** Called with the outcome of every submission. */
  readonly onSubmitted?: (outcome: Outcome<ProductUpdateResult>) => void;
}

/** What an operator calls this screen. The page decides where it goes. */
export const ProductUpdateFormLabel = "update";

/**
 * The form for `wamn-wms:product/update@1.0.0`.
 *
 * It renders what the operator fills and nothing else. The reserved inputs
 * come from the runtime at submit time, and the operator never sees them.
 */
export function ProductUpdateForm(props: ProductUpdateFormProps) {
  const [refusal, setRefusal] = createSignal<{ text: string; member: string | null } | null>(null);
  const [done, setDone] = createSignal(false);
  // The record this command changes, read when the form opens and again
  // when its key changes. The form sends the revision of this read, so a
  // change another writer makes after it refuses as a conflict.
  const [record, { refetch: readAgain }] = createResource(
    () => props.key,
    (key: ProductGetDetailInput) => get(props.transport, [key]),
  );

  const form = createForm(() => ({
    defaultValues: { ...props.initial } as Partial<ProductUpdateRequest>,
    onSubmit: async ({ value }: { value: Partial<ProductUpdateRequest> }) => {
      setDone(false);
      const checked = UPDATE_INPUT.safeParse(value);
      if (!checked.success) {
        const issue = checked.error.issues[0];
        setRefusal({
          text: issue?.message ?? "A value is not valid.",
          member: checkedMember(
            issue?.path as (string | number)[] | undefined,
            PRODUCT_UPDATE_REQUEST_FIELDS,
          ),
        });
        return;
      }
      let item = { ...value } as ProductUpdateRequest;
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
      announceOutcome(outcome, ProductUpdateFormLabel);
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
        <form.Field name={`change.productCode`}>
          {(field) => (
            <TextField
              label="product code"
              type="text"
              value={String(field().state.value ?? "")}
              onInput={(value) => field().handleChange(value)}
              error={refusalMarks(refusal()?.member ?? null, "change.product_code") ? (refusal()?.text ?? null) : null}
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
