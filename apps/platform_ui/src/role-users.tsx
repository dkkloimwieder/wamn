import { createSignal, onCleanup, Show, untrack } from "solid-js";
import { FieldError } from "@wfe/components/fields";
import { createForm } from "@wfe/form";
import { FormView } from "@wfe/generator";
import { role, user, userRole } from "@wamn/control-client";
import { ConfirmAction } from "./confirm-action";
import { MissingPage, Page, RecordLink } from "./page";
import { Records } from "./records";
import { value } from "./outcome";
import { rolePath } from "./application-roles";
import { applicationUsersPath } from "./application-users";
import type { ScreenProps } from "./shell";

export function RoleUsers(props: ScreenProps) {
  const name = untrack(() => props.params.role!);
  const [users, setUsers] = createSignal<user.UserListResult["users"]>([]);
  const [missing, setMissing] = createSignal(false);
  const [trouble, setTrouble] = createSignal<string>();
  let active = true;
  onCleanup(() => {
    active = false;
  });
  const refresh = async () => {
    const [people, roles] = await Promise.all([user.list(props.transport, [{}]), role.list(props.transport, [{}])]);
    const members = value(people).users.filter((entry) => entry.roles.includes(name));
    const exists = value(roles).roles.includes(name);
    if (active) {
      setUsers(members);
      setMissing(!exists);
    }
  };
  void untrack(refresh).catch((error: unknown) => {
    if (active) setTrouble(String(error));
  });
  return (
    <Show when={!missing()} fallback={<MissingPage />}>
      <div class="platform-table-screen">
        <Show when={trouble()}>{(text) => <FieldError role="alert">{text()}</FieldError>}</Show>
        <Records
          id="application-role-users"
          title={`Users with ${name}`}
          rows={users()}
          rowKey={(entry) => entry.id}
          refresh={refresh}
          create={{ label: "Grant to user", run: () => props.open(`${rolePath(name)}/users/grant`) }}
          columns={[
            { id: "name", header: "User name" },
            { id: "email", header: "Email", accessorKey: "email" },
            { id: "actions", header: "Actions", size: 80, sortable: false, filterable: false },
          ]}
          cells={{
            name: (cell) => (
              <RecordLink screen={props} path={`${applicationUsersPath}/${encodeURIComponent(cell.row.id)}`}>
                {cell.row.displayName ?? cell.row.email}
              </RecordLink>
            ),
            actions: (cell) => (
              <Show when={!(name === "admin" && cell.row.adminCovered)}>
                <ConfirmAction
                  label={`Revoke ${name} from ${cell.row.displayName ?? cell.row.email}`}
                  description={`Revoke ${name} from ${cell.row.displayName ?? cell.row.email}?`}
                  run={async () => {
                    value(
                      await userRole.revoke(props.transport, [
                        { requestId: crypto.randomUUID(), value: { userId: cell.row.id, role: name } },
                      ]),
                    );
                    await refresh();
                  }}
                />
              </Show>
            ),
          }}
        />
      </div>
    </Show>
  );
}

export function GrantRoleToUser(props: ScreenProps) {
  const name = untrack(() => props.params.role!);
  const returnTo = `${rolePath(name)}/users`;
  const [choices, setChoices] = createSignal<user.UserListResult["users"]>([]);
  const [loaded, setLoaded] = createSignal(false);
  const [missing, setMissing] = createSignal(false);
  const [trouble, setTrouble] = createSignal<string>();
  const [values, setValues] = createSignal({ userId: "" });
  let active = true;
  onCleanup(() => {
    active = false;
  });
  const load = async () => {
    const [people, roles] = await Promise.all([user.list(props.transport, [{}]), role.list(props.transport, [{}])]);
    const eligible = value(people).users.filter((entry) => !entry.roles.includes(name));
    const exists = value(roles).roles.includes(name);
    if (active) {
      setChoices(eligible);
      setMissing(!exists);
      setLoaded(true);
    }
  };
  void untrack(load).catch((error: unknown) => {
    if (active) setTrouble(String(error));
  });
  const form = createForm({
    values,
    defaults: () => ({ userId: "" }),
    onValuesChange: setValues,
    validate: (snapshot) => (choices().some((entry) => entry.id === snapshot.userId) ? {} : { form: "Select a user." }),
    onSubmit: async (snapshot, context) => {
      try {
        value(
          await userRole.grant(props.transport, [
            { requestId: crypto.randomUUID(), value: { userId: snapshot.userId, role: name } },
          ]),
        );
        if (!context.signal.aborted) props.open(returnTo);
        return { ok: true };
      } catch (error) {
        return { ok: false, errors: { form: String(error) } };
      }
    },
  });
  return (
    <Show when={!missing()} fallback={<MissingPage />}>
      <Page title={`Grant ${name} to user`}>
        <Show when={trouble()}>{(text) => <FieldError role="alert">{text()}</FieldError>}</Show>
        <Show when={loaded()}>
          <FormView
            description={{
              id: "grant-role-to-user",
              fields: [{ field: "userId", label: "User", control: "select" }],
              submit: { id: "grant", label: "Grant to user" },
              actions: [{ id: "cancel", label: "Cancel", variant: "ghost" }],
            }}
            bindings={{
              form,
              choices: {
                userId: () => choices().map((entry) => ({ value: entry.id, label: entry.displayName ?? entry.email })),
              },
              actions: { cancel: { run: () => props.open(returnTo) } },
            }}
          />
        </Show>
      </Page>
    </Show>
  );
}
