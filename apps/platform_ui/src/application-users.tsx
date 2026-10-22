import { createSignal, onCleanup, Show, untrack } from "solid-js";
import { FieldError } from "@wfe/components/fields";
import { createForm } from "@wfe/form";
import { FormView } from "@wfe/generator";
import { role, user, userRole } from "@wamn/control-client";
import { ConfirmAction } from "./confirm-action";
import { MissingPage, Page, RecordLink } from "./page";
import { Records } from "./records";
import { value } from "./outcome";
import type { ScreenProps } from "./shell";

export const applicationUsersPath = "administration/users";

export function ApplicationUsers(props: ScreenProps) {
  const [users, setUsers] = createSignal<user.UserListResult["users"]>([]);
  const [loaded, setLoaded] = createSignal(false);
  const [trouble, setTrouble] = createSignal<string>();
  let active = true;
  onCleanup(() => {
    active = false;
  });
  const refresh = async () => {
    const next = value(await user.list(props.transport, [{}]));
    if (active) {
      setUsers(next.users);
      setLoaded(true);
    }
  };
  void untrack(refresh).catch((error: unknown) => {
    if (active) setTrouble(String(error));
  });
  const person = () => users().find((entry) => entry.id === props.params.principal);
  return (
    <div class="platform-table-screen">
      <Show when={trouble()}>{(text) => <FieldError role="alert">{text()}</FieldError>}</Show>
      <Show when={!props.params.principal}>
        <Records
          id="application-users"
          title="Users"
          rows={users()}
          rowKey={(entry) => entry.id}
          refresh={refresh}
          columns={[
            { id: "name", header: "User name", size: 220 },
            { id: "email", header: "Email", accessorKey: "email", size: 280 },
            { id: "id", header: "User ID", accessorKey: "id", size: 160 },
            { id: "adminCovered", header: "Admin covered", accessorKey: "adminCovered", size: 140 },
          ]}
          formats={{ id: "short-hash", adminCovered: "boolean-symbol" }}
          cells={{
            name: (cell) => (
              <RecordLink screen={props} path={`${applicationUsersPath}/${encodeURIComponent(cell.row.id)}`}>
                {cell.row.displayName ?? cell.row.email}
              </RecordLink>
            ),
          }}
        />
      </Show>
      <Show when={props.params.principal && loaded()}>
        <Show when={person()} fallback={<MissingPage />}>
          {(current) => (
            <Records
              id="application-user-roles"
              title={`Roles for ${current().displayName ?? current().email}`}
              rows={current().roles}
              rowKey={(name) => name}
              refresh={refresh}
              create={{
                label: "Grant role",
                run: () => props.open(`${applicationUsersPath}/${encodeURIComponent(current().id)}/grant`),
              }}
              columns={[
                { id: "role", header: "Role", accessorFn: (name) => name },
                { id: "actions", header: "Actions" },
              ]}
              cells={{
                actions: (cell) => (
                  <Show when={!(cell.row === "admin" && current().adminCovered)}>
                    <ConfirmAction
                      label={`Revoke role ${cell.row}`}
                      description={`Revoke ${cell.row} from ${current().displayName ?? current().email}?`}
                      run={async () => {
                        value(
                          await userRole.revoke(props.transport, [
                            { requestId: crypto.randomUUID(), value: { userId: current().id, role: cell.row } },
                          ]),
                        );
                        await refresh();
                      }}
                    />
                  </Show>
                ),
              }}
            />
          )}
        </Show>
      </Show>
    </div>
  );
}

export function GrantApplicationRole(props: ScreenProps) {
  const userId = untrack(() => props.params.principal!);
  const returnTo = `${applicationUsersPath}/${encodeURIComponent(userId)}`;
  const [choices, setChoices] = createSignal<string[]>([]);
  const [loaded, setLoaded] = createSignal(false);
  const [missing, setMissing] = createSignal(false);
  const [trouble, setTrouble] = createSignal<string>();
  const [values, setValues] = createSignal({ role: "" });
  let active = true;
  onCleanup(() => {
    active = false;
  });
  const load = async () => {
    const [people, roles] = await Promise.all([user.list(props.transport, [{}]), role.list(props.transport, [{}])]);
    const person = value(people).users.find((entry) => entry.id === userId);
    const names = value(roles).roles;
    if (active) {
      setMissing(!person);
      setChoices(names.filter((name) => !person?.roles.includes(name)));
      setLoaded(true);
    }
  };
  void untrack(load).catch((error: unknown) => {
    if (active) setTrouble(String(error));
  });
  const form = createForm({
    values,
    defaults: () => ({ role: "" }),
    onValuesChange: setValues,
    validate: (snapshot) => (choices().includes(snapshot.role) ? {} : { form: "Select a role." }),
    onSubmit: async (snapshot, context) => {
      try {
        value(
          await userRole.grant(props.transport, [
            { requestId: crypto.randomUUID(), value: { userId, role: snapshot.role } },
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
      <Page title="Grant role">
        <Show when={trouble()}>{(text) => <FieldError role="alert">{text()}</FieldError>}</Show>
        <Show when={loaded()}>
          <FormView
            description={{
              id: "grant-application-role",
              fields: [{ field: "role", label: "Role", control: "select" }],
              submit: { id: "grant", label: "Grant role" },
              actions: [{ id: "cancel", label: "Cancel", variant: "ghost" }],
            }}
            bindings={{
              form,
              choices: { role: () => choices().map((name) => ({ value: name, label: name })) },
              actions: { cancel: { run: () => props.open(returnTo) } },
            }}
          />
        </Show>
      </Page>
    </Show>
  );
}
