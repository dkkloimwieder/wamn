import { createSignal, onCleanup, Show, untrack } from "solid-js";
import { Checkbox } from "@wfe/components";
import { FieldError } from "@wfe/components/fields";
import { createForm } from "@wfe/form";
import { FormView } from "@wfe/generator";
import { permission, role } from "@wamn/control-client";
import { ConfirmAction } from "./confirm-action";
import { MissingPage, Page, RecordLink } from "./page";
import { Records } from "./records";
import { value } from "./outcome";
import type { ScreenProps } from "./shell";

export const rolesPath = "administration/roles";
export const rolePath = (name: string) => `${rolesPath}/${encodeURIComponent(name)}`;
type Operation = permission.PermissionListResult["operations"][number];
export const grantedOperations = (list: permission.PermissionListResult) =>
  list.operations.filter((op) => op.served && (list.admin || op.selected));

export function ApplicationRoles(props: ScreenProps & { overview?: boolean }) {
  const [permissions, setPermissions] = createSignal<permission.PermissionListResult[]>([]);
  const [trouble, setTrouble] = createSignal<string>();
  let active = true;
  onCleanup(() => {
    active = false;
  });
  const refresh = async () => {
    const names = value(await role.list(props.transport, [{}])).roles;
    const lists = await Promise.all(
      names.map(async (name) => value(await permission.list(props.transport, [{ role: name }]))),
    );
    if (active) setPermissions(lists);
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
  return (
    <div class="platform-table-screen">
      <Show when={trouble()}>{(text) => <FieldError role="alert">{text()}</FieldError>}</Show>
      <Show
        when={!props.overview}
        fallback={
          <Show when={permissions()} keyed>
            {(lists) => (
              <Records<Operation>
                id="application-permissions-overview"
                title="Permissions overview"
                rows={rows()}
                rowKey={(op) => op.operation}
                refresh={refresh}
                columns={[
                  { id: "operation", header: "Operation", accessorKey: "operation", size: 420 },
                  ...lists.map((list) => ({
                    id: `role:${list.role}`,
                    header: list.role,
                    size: 150,
                    accessorFn: (op: Operation) =>
                      list.admin || !!list.operations.find((entry) => entry.operation === op.operation)?.selected,
                  })),
                ]}
                formats={Object.fromEntries(lists.map((list) => [`role:${list.role}`, "boolean-symbol" as const]))}
              />
            )}
          </Show>
        }
      >
        <Records
          id="application-roles"
          title="Roles"
          rows={permissions()}
          rowKey={(list) => list.role}
          refresh={refresh}
          create={{ label: "Create role", run: () => props.open(`${rolesPath}/create`) }}
          columns={[
            { id: "role", header: "Role", accessorKey: "role" },
            { id: "operations", header: "Granted operations", accessorFn: (list) => grantedOperations(list).length },
            { id: "actions", header: "Actions" },
          ]}
          cells={{
            role: (cell) => (
              <RecordLink screen={props} path={`${rolePath(cell.row.role)}/permissions`}>
                {cell.row.role}
              </RecordLink>
            ),
            actions: (cell) => (
              <Show when={cell.row.role !== "admin"}>
                <ConfirmAction
                  label={`Delete role ${cell.row.role}`}
                  description={`Delete the role ${cell.row.role} and its permissions?`}
                  run={async () => {
                    value(
                      await role.delete_(props.transport, [
                        { requestId: crypto.randomUUID(), value: { role: cell.row.role } },
                      ]),
                    );
                    await refresh();
                  }}
                />
              </Show>
            ),
          }}
        />
      </Show>
    </div>
  );
}

export function PermissionsOverview(props: ScreenProps) {
  return <ApplicationRoles {...props} overview />;
}

export function RolePermissions(props: ScreenProps) {
  const name = untrack(() => props.params.role!);
  const [list, setList] = createSignal<permission.PermissionListResult>();
  const [missing, setMissing] = createSignal(false);
  const [pending, setPending] = createSignal<ReadonlySet<string>>(new Set());
  const [trouble, setTrouble] = createSignal<string>();
  const [result, setResult] = createSignal<string>();
  let active = true;
  onCleanup(() => {
    active = false;
  });
  const refresh = async () => {
    if (pending().size) return;
    const names = value(await role.list(props.transport, [{}])).roles;
    const next = names.includes(name) ? value(await permission.list(props.transport, [{ role: name }])) : undefined;
    if (active) {
      setMissing(!next);
      setList(next);
    }
  };
  void untrack(refresh).catch((error: unknown) => {
    if (active) setTrouble(String(error));
  });
  const selected = (op: Operation) => !!list()?.admin || op.selected;
  const setSelected = (operation: string, checked: boolean) =>
    setList(
      (previous) =>
        previous && {
          ...previous,
          operations: previous.operations.map((op) => (op.operation === operation ? { ...op, selected: checked } : op)),
        },
    );
  const change = async (op: Operation, checked: boolean) => {
    const previous = selected(op);
    if (
      list()?.admin ||
      name === "admin" ||
      op.adminOnly ||
      !op.grantable ||
      pending().has(op.operation) ||
      previous === checked
    )
      return;
    setPending((keys) => new Set([...keys, op.operation]));
    setTrouble(undefined);
    setResult(undefined);
    setSelected(op.operation, checked);
    try {
      value(
        await (checked ? permission.grant : permission.revoke)(props.transport, [
          { requestId: crypto.randomUUID(), value: { role: name, operation: op.operation } },
        ]),
      );
      if (active) setResult(`${checked ? "Granted" : "Revoked"} ${op.operation} ${checked ? "to" : "from"} ${name}.`);
    } catch (error) {
      if (active) {
        setSelected(op.operation, previous);
        setTrouble(String(error));
      }
    } finally {
      if (active) setPending((keys) => new Set([...keys].filter((entry) => entry !== op.operation)));
    }
  };
  return (
    <Show when={!missing()} fallback={<MissingPage />}>
      <div class="platform-table-screen">
        <Show when={trouble()}>{(text) => <FieldError role="alert">{text()}</FieldError>}</Show>
        <Show when={result()}>{(text) => <p role="status">{text()}</p>}</Show>
        <Records<Operation>
          id="application-role-permissions"
          title={`Permissions for ${name}`}
          rows={list()?.operations.filter((op) => op.served) ?? []}
          rowKey={(op) => op.operation}
          refresh={refresh}
          columns={[
            { id: "operation", header: "Operation", accessorKey: "operation", size: 420 },
            { id: "granted", header: "Granted", size: 150 },
          ]}
          cells={{
            granted: (cell) => (
              <Checkbox
                checked={selected(cell.row)}
                readOnly={name === "admin" || list()?.admin}
                disabled={cell.row.adminOnly || !cell.row.grantable || pending().has(cell.row.operation)}
                onChange={(checked) => void change(cell.row, checked)}
              >
                <Checkbox.Input aria-label={`${cell.row.operation} ${name}`} />
                <Checkbox.Control>
                  <Checkbox.Indicator />
                </Checkbox.Control>
              </Checkbox>
            ),
          }}
        />
      </div>
    </Show>
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
