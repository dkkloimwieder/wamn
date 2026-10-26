import { createSignal, onCleanup, Show } from "solid-js";
import { createForm, type FormErrors } from "@wfe/form";
import { FormView } from "@wfe/generator";
import { DashboardPanel } from "@wfe/components/layout";
import { FieldError } from "@wfe/components";
import { filledValues, type ScreenProps } from "@wamn/platform-ui";
import { newIdempotencyKey, newRequestId, occurredAt, uuidv7 } from "@wamn/web-runtime";
import * as purchase from "./generated/wamn_receiving/purchase_order.js";
import * as supplier from "./generated/wamn_receiving/supplier.js";
import * as receiving from "./generated/wamn_receiving/receiving.js";
import { PurchaseOrderUpdateForm } from "./generated/wamn_receiving/components/purchase_order.js";
import { SupplierCreateForm } from "./generated/wamn_receiving/components/supplier.js";
import {
  ReceivingRecordReceiptForm,
  type ReceivingRecordReceiptFormValues,
} from "./generated/wamn_receiving/components/receiving.js";
import { Selector } from "./selector";
import { invoke, Refusal, type RecordValue } from "./data";

function message(error: unknown) {
  return error instanceof Error ? error.message : String(error);
}

export function SupplierForm(props: ScreenProps) {
  const initial = { name: filledValues<{ name?: string }>(props.search).name ?? "" };
  const [values, setValues] = createSignal(initial);
  let intent: { name: string; key: string; id: string } | undefined;
  const form = createForm({
    values,
    defaults: () => initial,
    onValuesChange: setValues,
    validate: (snapshot) => (snapshot.name.trim() ? {} : { fields: { name: "Enter a value." } }),
    onSubmit: async (snapshot, { signal }) => {
      if (intent?.name !== snapshot.name) intent = { name: snapshot.name, key: newIdempotencyKey(), id: uuidv7() };
      try {
        await invoke(props.transport, supplier.SUPPLIER_CREATE_ROUTE, {
          ...snapshot,
          id: intent.id,
          idempotency_key: intent.key,
          request_id: newRequestId(),
        });
        if (!signal.aborted) props.open("suppliers");
        return { ok: true };
      } catch (error) {
        return { ok: false, errors: { form: message(error) } };
      }
    },
  });
  return (
    <DashboardPanel>
      <FormView
        description={{ ...SupplierCreateForm, actions: [{ id: "cancel", label: "Cancel", variant: "ghost" }] }}
        bindings={{ form, actions: { cancel: { run: () => props.open("suppliers") } } }}
      />
    </DashboardPanel>
  );
}

export function PurchaseOrderForm(props: ScreenProps) {
  const [record, setRecord] = createSignal<RecordValue>();
  const [error, setError] = createSignal<string>();
  let active = true;
  onCleanup(() => {
    active = false;
  });
  void invoke<RecordValue>(props.transport, purchase.PURCHASE_ORDER_GET_ROUTE, { id: props.params.id! }).then(
    (record) => {
      if (active) setRecord(record);
    },
    (error) => {
      if (active) setError(message(error));
    },
  );
  return (
    <DashboardPanel>
      <Show when={error()}>{(error) => <FieldError role="alert">{error()}</FieldError>}</Show>
      <Show when={record()}>{(record) => <PurchaseOrderEditor {...props} record={record()} />}</Show>
    </DashboardPanel>
  );
}

function PurchaseOrderEditor(props: ScreenProps & { record: RecordValue }) {
  const initial = { "change.supplier_id": String(props.record.supplier_id) as string | undefined };
  const revision = props.record.row_version!;
  const [values, setValues] = createSignal(initial);
  const form = createForm({
    values,
    defaults: () => initial,
    onValuesChange: setValues,
    validate: (snapshot) =>
      snapshot["change.supplier_id"] ? {} : { fields: { "change.supplier_id": "Choose a value." } },
    onSubmit: async (snapshot, { signal }) => {
      try {
        await invoke(props.transport, purchase.PURCHASE_ORDER_UPDATE_ROUTE, {
          request_id: newRequestId(),
          id: props.params.id!,
          expected_row_version: revision,
          change: { supplier_id: snapshot["change.supplier_id"]! },
        });
        if (!signal.aborted) props.open("purchase-orders");
        return { ok: true };
      } catch (error) {
        return { ok: false, errors: { form: message(error) } };
      }
    },
  });
  return (
    <FormView
      description={{ ...PurchaseOrderUpdateForm, actions: [{ id: "cancel", label: "Cancel", variant: "ghost" }] }}
      bindings={{
        form,
        actions: { cancel: { run: () => props.open("purchase-orders") } },
        fields: {
          "change.supplier_id": (field) => (
            <Selector
              {...field}
              field="value"
              transport={props.transport}
              reference={PurchaseOrderUpdateForm.presentation.references["change.supplier_id"]}
              label={PurchaseOrderUpdateForm.fields[0].label}
            />
          ),
        },
      }}
    />
  );
}

type Line = ReceivingRecordReceiptFormValues["value.line[]"][number] & { key: string };
type ReceiptValues = Omit<ReceivingRecordReceiptFormValues, "value.line[]"> & { "value.line[]": Line[] };

export function ReceiptForm(props: ScreenProps) {
  const fill = filledValues<{
    value?: {
      purchaseOrderId?: string;
      receiptReference?: string;
      line?: { locationId?: string; purchaseOrderLineId?: string; quantity?: string }[];
    };
  }>(props.search);
  const newLine = (): Line => ({ key: crypto.randomUUID(), location_id: "", purchase_order_line_id: "", quantity: "" });
  const initial: ReceiptValues = {
    "value.purchase_order_id": fill.value?.purchaseOrderId ?? "",
    "value.receipt_reference": fill.value?.receiptReference ?? "",
    "value.line[]": fill.value?.line?.map((line) => ({
      ...newLine(),
      location_id: line.locationId ?? "",
      purchase_order_line_id: line.purchaseOrderLineId ?? "",
      quantity: line.quantity ?? "",
    })) ?? [newLine()],
  };
  const [values, setValues] = createSignal(initial);
  // An unchanged retry keeps its intent. A changed submission receives a new key and timestamp.
  let intent: { snapshot: string; key: string; time: string } | undefined;
  const form = createForm<ReceiptValues>({
    values,
    defaults: () => initial,
    onValuesChange: (next) =>
      setValues((old) =>
        old["value.purchase_order_id"] === next["value.purchase_order_id"]
          ? next
          : { ...next, "value.line[]": next["value.line[]"].map((line) => ({ ...line, purchase_order_line_id: "" })) },
      ),
    validate: (snapshot) => {
      const fields: Record<string, string> = {};
      for (const key of ["value.purchase_order_id", "value.receipt_reference"] as const)
        if (!snapshot[key].trim()) fields[key] = "Enter a value.";
      if (!snapshot["value.line[]"].length) fields["value.line[]"] = "Add a row.";
      const rows = Object.fromEntries(
        snapshot["value.line[]"].map((line) => [
          line.key,
          {
            fields: {
              ...(!line.location_id ? { location_id: "Choose a value." } : {}),
              ...(!line.purchase_order_line_id ? { purchase_order_line_id: "Choose a value." } : {}),
              ...(!/^(?:\d+(?:\.\d*)?|\.\d+)$/.test(line.quantity) || !/[1-9]/.test(line.quantity)
                ? { quantity: "Enter a quantity greater than zero." }
                : {}),
            },
          },
        ]),
      );
      return { fields, rows: { "value.line[]": rows } };
    },
    onSubmit: async (snapshot, { signal }) => {
      const value = {
        purchase_order_id: snapshot["value.purchase_order_id"],
        receipt_reference: snapshot["value.receipt_reference"],
        line: snapshot["value.line[]"].map(({ key: _key, ...line }) => line),
      };
      const serialized = JSON.stringify(value);
      if (intent?.snapshot !== serialized)
        intent = { snapshot: serialized, key: newIdempotencyKey(), time: occurredAt() };
      try {
        await invoke(props.transport, receiving.RECEIVING_RECORD_RECEIPT_ROUTE, {
          request_id: newRequestId(),
          value: { ...value, idempotency_key: intent.key, occurred_at: intent.time },
        });
        if (!signal.aborted) props.open("receipts");
        return { ok: true };
      } catch (error) {
        const errors: FormErrors<ReceiptValues> = { form: message(error) };
        if (error instanceof Refusal && error.outcome.status === "refused") {
          const detail = error.outcome.detail as RecordValue | null;
          const path = String(detail?.field ?? "");
          const match = /^value\.line\[(\d+)\]\.(.+)$/.exec(path);
          if (match) {
            const key = snapshot["value.line[]"][Number(match[1])]?.key;
            if (key)
              return {
                ok: false,
                errors: { ...errors, rows: { "value.line[]": { [key]: { fields: { [match[2]!]: message(error) } } } } },
              };
          }
          if (path in snapshot) return { ok: false, errors: { ...errors, fields: { [path]: message(error) } } };
        }
        return { ok: false, errors };
      }
    },
  });
  return (
    <DashboardPanel>
      <FormView
        description={{ ...ReceivingRecordReceiptForm, actions: [{ id: "cancel", label: "Cancel", variant: "ghost" }] }}
        bindings={{
          form,
          actions: { cancel: { run: () => props.open("receipts") } },
          fields: {
            "value.purchase_order_id": (field) => (
              <Selector
                {...field}
                field="value"
                transport={props.transport}
                reference={ReceivingRecordReceiptForm.presentation.references["value.purchase_order_id"]}
                label={ReceivingRecordReceiptForm.fields[0].label}
              />
            ),
          },
          repeated: {
            "value.line[]": {
              rowKey: (line) => line.key,
              createRow: newLine,
              fields: {
                location_id: (field) => (
                  <Selector
                    {...field}
                    field="value"
                    transport={props.transport}
                    reference={ReceivingRecordReceiptForm.presentation.references["value.line[].location_id"]}
                    label={ReceivingRecordReceiptForm.fields[2].fields[0].label}
                  />
                ),
                purchase_order_line_id: (field) => (
                  <Selector
                    {...field}
                    field="value"
                    transport={props.transport}
                    reference={
                      ReceivingRecordReceiptForm.presentation.references["value.line[].purchase_order_line_id"]
                    }
                    label={ReceivingRecordReceiptForm.fields[2].fields[1].label}
                    unavailable={!values()["value.purchase_order_id"]}
                    input={{ purchase_order_id: values()["value.purchase_order_id"] }}
                  />
                ),
              },
            },
          },
        }}
      />
    </DashboardPanel>
  );
}
