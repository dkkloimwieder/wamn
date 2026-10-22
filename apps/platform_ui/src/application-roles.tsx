import { createSignal, For, onCleanup, Show, untrack } from "solid-js";
import { Checkbox } from "@wfe/components";
import { FieldError } from "@wfe/components/fields";
import { createForm } from "@wfe/form";
import { FormView } from "@wfe/generator";
import { permission, role } from "@wamn/control-client";
import { ConfirmAction } from "./confirm-action";
import { Page } from "./page";
import { Records } from "./records";
import { value } from "./outcome";
import type { ScreenProps } from "./shell";

export const rolesPath = "administration/roles";
type Operation = permission.PermissionListResult["operations"][number];

export function ApplicationRoles(props: ScreenProps) {
  const [roles, setRoles] = createSignal<readonly string[]>([]);
  const [permissions, setPermissions] = createSignal<permission.PermissionListResult[]>([]);
  const [pending, setPending] = createSignal<ReadonlySet<string>>(new Set());
  const [trouble, setTrouble] = createSignal<string>();
  const [result, setResult] = createSignal<string>();
  let active = true;
  onCleanup(() => {
    active = false;
  });
  const refresh = async () => {
    const names = value(await role.list(props.transport, [{}])).roles;
    const lists = await Promise.all(
      names.map(async (name) => value(await permission.list(props.transport, [{ role: name }]))),
    );
    if (active) {
      setPermissions(lists);
      setRoles(names);
    }
  };
  void untrack(refresh).catch((error: unknown) => {
    if (active) setTrouble(String(error));
  });
  const rows = () => [
    ...new Map(
      permissions().flatMap((list) =>
        list.operations.filter((op) => op.served).map((op) => [op.operation, op] as const),
      ),
    ).values(),
  ];
  const selected = (name: string, operation: string) => {
    const list = permissions().find((entry) => entry.role === name);
    return list?.admin || (list?.operations.find((op) => op.operation === operation)?.selected ?? false);
  };
  const setSelected = (name: string, operation: string, checked: boolean) =>
    setPermissions((lists) =>
      lists.map((list) =>
        list.role === name
          ? {
              ...list,
              operations: list.operations.map((op) => (op.operation === operation ? { ...op, selected: checked } : op)),
            }
          : list,
      ),
    );
  const change = async (name: string, op: Operation, checked: boolean) => {
    const key = JSON.stringify([name, op.operation]);
    const previous = selected(name, op.operation);
    if (name === "admin" || op.adminOnly || !op.grantable || pending().has(key) || previous === checked) return;
    setPending((keys) => new Set([...keys, key]));
    setTrouble(undefined);
    setResult(undefined);
    setSelected(name, op.operation, checked);
    try {
      value(
        await (checked ? permission.grant : permission.revoke)(props.transport, [
          { requestId: crypto.randomUUID(), value: { role: name, operation: op.operation } },
        ]),
      );
      if (active) setResult(`${checked ? "Granted" : "Revoked"} ${op.operation} ${checked ? "to" : "from"} ${name}.`);
    } catch (error) {
      if (active) {
        setSelected(name, op.operation, previous);
        setTrouble(String(error));
      }
    } finally {
      if (active) setPending((keys) => new Set([...keys].filter((entry) => entry !== key)));
    }
  };
  return (
    <div class="platform-table-screen">
      <Show when={trouble()}>{(text) => <FieldError role="alert">{text()}</FieldError>}</Show>
      <Show when={result()}>{(text) => <p role="status">{text()}</p>}</Show>
      <Show when={roles()} keyed>
        {(names) => (
          <Records<Operation>
            id="application-roles"
            title="Roles"
            rows={rows()}
            rowKey={(op) => op.operation}
            refresh={async () => {
              if (!pending().size) await refresh();
            }}
            create={{ label: "Create role", run: () => props.open(`${rolesPath}/create`) }}
            toolbarActions={
              <For each={names.filter((name) => name !== "admin")}>
                {(name) => (
                  <ConfirmAction
                    label={`Delete role ${name}`}
                    description={`Delete the role ${name} and its permissions?`}
                    run={async () => {
                      value(
                        await role.delete_(props.transport, [
                          { requestId: crypto.randomUUID(), value: { role: name } },
                        ]),
                      );
                      await refresh();
                    }}
                  />
                )}
              </For>
            }
            columns={[
              { id: "operation", header: "Operation", accessorKey: "operation", size: 420 },
              ...names.map((name) => ({ id: `role:${name}`, header: name, size: 150 })),
            ]}
            cells={Object.fromEntries(
              names.map((name) => [
                `role:${name}`,
                (cell: { row: Operation }) => (
                  <Checkbox
                    checked={selected(name, cell.row.operation)}
                    readOnly={name === "admin"}
                    disabled={
                      cell.row.adminOnly ||
                      !cell.row.grantable ||
                      pending().has(JSON.stringify([name, cell.row.operation]))
                    }
                    onChange={(checked) => void change(name, cell.row, checked)}
                  >
                    <Checkbox.Input aria-label={`${cell.row.operation} ${name}`} />
                    <Checkbox.Control>
                      <Checkbox.Indicator />
                    </Checkbox.Control>
                  </Checkbox>
                ),
              ]),
            )}
          />
        )}
      </Show>
    </div>
  );
}

export function CreateRole(props: ScreenProps) {
  const [values, setValues] = createSignal({ role: "" });
  const form = createForm({
    values,
    defaults: () => ({ role: "" }),
    onValuesChange: setValues,
    validate: (snapshot) => (snapshot.role.trim() ? {} : { form: "Enter a role name." }),
    onSubmit: async (snapshot, context) => {
      try {
        value(await role.create(props.transport, [{ requestId: crypto.randomUUID(), value: snapshot }]));
        if (!context.signal.aborted) props.open(rolesPath);
        return { ok: true };
      } catch (error) {
        return { ok: false, errors: { form: String(error) } };
      }
    },
  });
  return (
    <Page title="Create role">
      <FormView
        description={{
          id: "create-role",
          fields: [{ field: "role", label: "Role", control: "text" }],
          submit: { id: "create", label: "Create role" },
          actions: [{ id: "cancel", label: "Cancel", variant: "ghost" }],
        }}
        bindings={{ form, actions: { cancel: { run: () => props.open(rolesPath) } } }}
      />
    </Page>
  );
}
