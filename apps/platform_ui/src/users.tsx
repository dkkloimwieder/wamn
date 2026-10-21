import { createSignal, onCleanup, Show } from "solid-js";
import type { JSX } from "@solidjs/web";
import { Button, FieldError, Input } from "@wfe/components/fields";
import { Field, FieldValue, FormGrid, Label } from "@wfe/components";
import { FormView, type CustomFieldProps, type FormDescription } from "@wfe/generator";
import { Records } from "./records";
import { emailError } from "./email";
import { createForm } from "@wfe/form";
import { environment, member, orgAdmin, project, projectAdmin, user } from "@wamn/control-org-client";
import type { Outcome } from "@wamn/web-runtime";
import { Page } from "./page";
import { value } from "./outcome";
import type { ScreenProps, ShellSection } from "./shell";

export function Users(props: ScreenProps): JSX.Element {
  const [users, setUsers] = createSignal<readonly user.UserListResultUsers[]>([]);
  const [invitation, setInvitation] = createSignal<user.UserInviteResult>();
  const [trouble, setTrouble] = createSignal<string>();
  const [pending, setPending] = createSignal(false);
  let active = true;
  onCleanup(() => {
    active = false;
  });
  const refresh = async () => {
    const next = value(await user.list(props.transport, [{}])).users;
    if (active) setUsers(next);
  };
  void refresh().catch((error: unknown) => {
    if (active) setTrouble(String(error));
  });
  const write = async (invoke: () => Promise<Outcome<unknown>>) => {
    if (pending()) return;
    setPending(true);
    setTrouble(undefined);
    try {
      value(await invoke());
      await refresh();
    } catch (error) {
      if (active) setTrouble(String(error));
    } finally {
      if (active) setPending(false);
    }
  };
  const defaults = { email: "", displayName: "" };
  const Email = (field: CustomFieldProps<typeof defaults, "email">) => <Input {...field} type="email" />;
  const [values, setValues] = createSignal(defaults);
  const form = createForm({
    values,
    defaults: () => defaults,
    onValuesChange: setValues,
    validate: (snapshot) => {
      const email = emailError(snapshot.email);
      if (email) return { fields: { email } };
      return snapshot.email.trim() && snapshot.displayName.trim()
        ? {}
        : { form: "Enter an email address and a display name." };
    },
    onSubmit: async (snapshot) => {
      try {
        const invited = value(
          await user.invite(props.transport, [
            {
              requestId: crypto.randomUUID(),
              value: {
                ...snapshot,
                memberships: [],
                projectAdmins: [],
                orgAdmin: false,
              },
            },
          ]),
        );
        await refresh();
        if (active) setInvitation(invited);
        return { ok: true };
      } catch (error) {
        return { ok: false, errors: { form: String(error) } };
      }
    },
  });
  // Present the existing write lock without creating another form coordinator.
  const invitationForm = Object.create(form, {
    pending: { get: () => form.pending || pending() },
  }) as typeof form;
  return (
    <Page title="Users">
      <Show when={trouble()}>{(text) => <FieldError role="alert">{text()}</FieldError>}</Show>
      <Show when={props.holds(user.USER_INVITE_ROUTE.operation)}>
        <FormView
          description={
            {
              id: "invite-user",
              fields: [
                { field: "email", label: "Email", control: "custom" },
                { field: "displayName", label: "Display name", control: "text" },
              ],
              submit: { id: "invite-user", label: "Invite user" },
            } as const satisfies FormDescription<typeof defaults>
          }
          bindings={{
            form: invitationForm,
            fields: { email: Email },
          }}
        />
        <Show when={form.phase === "success"}>
          <p role="status">{invitation()?.invited ? "User invited." : "User is already enrolled."}</p>
        </Show>
      </Show>
      <Records
        id="users"
        title="Users"
        rows={users()}
        rowKey={(person) => person.principalId}
        columns={[
          { id: "name", header: "Name", size: 180 },
          { id: "email", header: "Email", accessorKey: "email", size: 250 },
          { id: "principalId", header: "User ID", accessorKey: "principalId", size: 120 },
          { id: "state", header: "State", size: 170 },
          { id: "role", header: "Role", size: 120 },
          { id: "activate", header: "Account", size: 170 },
          { id: "admin", header: "Administration", size: 190 },
        ]}
        formats={{ principalId: "short-hash" }}
        cells={{
          name: (cell) => (
            <Button onClick={() => props.open(`users/${encodeURIComponent(cell.row.principalId)}`)}>
              {cell.row.displayName}
            </Button>
          ),
          state: (cell) => <>User state: {cell.row.status}</>,
          role: (cell) => <>{cell.row.orgAdmin ? "Org admin" : "User"}</>,
          activate: (cell) => (
            <Show
              when={props.holds(
                cell.row.status === "active"
                  ? user.USER_DEACTIVATE_ROUTE.operation
                  : user.USER_ACTIVATE_ROUTE.operation,
              )}
            >
              <Button
                disabled={pending() || form.pending}
                onClick={() =>
                  void write(() =>
                    (cell.row.status === "active" ? user.deactivate : user.activate)(props.transport, [
                      {
                        requestId: crypto.randomUUID(),
                        value: { principalId: cell.row.principalId },
                      },
                    ]),
                  )
                }
              >
                {cell.row.status === "active" ? "Deactivate user" : "Activate user"}
              </Button>
            </Show>
          ),
          admin: (cell) => (
            <Show
              when={props.holds(
                cell.row.orgAdmin
                  ? orgAdmin.ORG_ADMIN_REVOKE_ROUTE.operation
                  : orgAdmin.ORG_ADMIN_GRANT_ROUTE.operation,
              )}
            >
              <Button
                disabled={pending() || form.pending}
                onClick={() =>
                  void write(() =>
                    (cell.row.orgAdmin ? orgAdmin.revoke : orgAdmin.grant)(props.transport, [
                      {
                        requestId: crypto.randomUUID(),
                        value: { principalId: cell.row.principalId },
                      },
                    ]),
                  )
                }
              >
                {cell.row.orgAdmin ? "Revoke org admin" : "Grant org admin"}
              </Button>
            </Show>
          ),
        }}
      />
    </Page>
  );
}

interface ProjectAccess {
  readonly project: string;
  readonly state: project.ProjectListResultProjects["state"];
  readonly environments: readonly environment.EnvironmentListResultEnvironments[];
  readonly member?: member.MemberListResultMembers;
}

export function User(props: ScreenProps): JSX.Element {
  const principalId = props.params.principal!;
  const [person, setPerson] = createSignal<user.UserListResultUsers>();
  const [projects, setProjects] = createSignal<readonly ProjectAccess[]>([]);
  const [trouble, setTrouble] = createSignal<string>();
  const [pending, setPending] = createSignal(false);
  let active = true;
  onCleanup(() => {
    active = false;
  });
  const refresh = async () => {
    const [people, listed] = await Promise.all([user.list(props.transport, [{}]), project.list(props.transport, [{}])]);
    const found = value(people).users.find((entry) => entry.principalId === principalId);
    if (!found) throw new Error("The org has no such user.");
    const access = await Promise.all(
      value(listed).projects.map(async ({ name, state }) => {
        const [members, environments] = await Promise.all([
          member.list(props.transport, [{ project: name }]),
          environment.list(props.transport, [{ project: name }]),
        ]);
        return {
          project: name,
          state,
          environments: value(environments).environments,
          member: value(members).members.find((entry) => entry.principalId === principalId),
        };
      }),
    );
    if (active) {
      setPerson(found);
      setProjects(access);
    }
  };
  void refresh().catch((error: unknown) => {
    if (active) setTrouble(String(error));
  });
  const write = async (invoke: () => Promise<Outcome<unknown>>) => {
    if (pending()) return;
    setPending(true);
    setTrouble(undefined);
    try {
      value(await invoke());
      await refresh();
    } catch (error) {
      if (active) setTrouble(String(error));
    } finally {
      if (active) setPending(false);
    }
  };
  const defaults = { project: "", env: "" };
  const [values, setValues] = createSignal(defaults);
  const form = createForm({
    values,
    defaults: () => defaults,
    onValuesChange: (next) => setValues(next.project === values().project ? next : { ...next, env: "" }),
    validate: (snapshot) => (snapshot.project && snapshot.env ? {} : { form: "Select a project and an environment." }),
    onSubmit: async (snapshot) => {
      try {
        value(
          await member.grant(props.transport, [
            { requestId: crypto.randomUUID(), value: { ...snapshot, principalId } },
          ]),
        );
        await refresh();
        return { ok: true };
      } catch (error) {
        return { ok: false, errors: { form: String(error) } };
      }
    },
  });
  // Present the existing write lock without creating another form coordinator.
  const membershipForm = Object.create(form, {
    pending: { get: () => form.pending || pending() },
  }) as typeof form;
  return (
    <Page title={person()?.displayName ?? "User"}>
      <Show when={trouble()}>{(text) => <FieldError role="alert">{text()}</FieldError>}</Show>
      <Show when={person()}>
        {(current) => (
          <>
            <p>{current().email}</p>
            <p>User state: {current().status}</p>
            <FormGrid columns={2}>
              <Field>
                <Label>User ID</Label>
                <FieldValue value={current().principalId} format="short-hash" />
              </Field>
              <Field>
                <Label>Org admin</Label>
                <FieldValue value={current().orgAdmin} format="boolean-symbol" />
              </Field>
            </FormGrid>
            <Show when={props.holds(member.MEMBER_GRANT_ROUTE.operation, form.values.project || undefined)}>
              <FormView
                description={
                  {
                    id: "user-membership",
                    fields: [
                      { field: "project", label: "Project", control: "select" },
                      { field: "env", label: "Environment", control: "select" },
                    ],
                    submit: { id: "grant-membership", label: "Grant membership" },
                  } as const satisfies FormDescription<typeof defaults>
                }
                bindings={{
                  form: membershipForm,
                  choices: {
                    project: () => projects().map((entry) => ({ value: entry.project, label: entry.project })),
                    env: () =>
                      (projects().find((entry) => entry.project === form.values.project)?.environments ?? []).map(
                        (env) => ({ value: env.name, label: env.name }),
                      ),
                  },
                }}
              />
              <Show when={form.phase === "success"}>
                <p role="status">Membership granted.</p>
              </Show>
            </Show>
            <Records
              id="project-access"
              title="Project access"
              rows={projects()}
              rowKey={(entry) => entry.project}
              columns={[
                { id: "project", header: "Project", accessorKey: "project", size: 200 },
                { id: "state", header: "State", accessorKey: "state", size: 100 },
                {
                  id: "displayName",
                  header: "Display name",
                  accessorFn: (entry) => entry.member?.displayName,
                  size: 180,
                },
                { id: "email", header: "Email", accessorFn: (entry) => entry.member?.email, size: 250 },
                { id: "principalId", header: "User ID", accessorFn: (entry) => entry.member?.principalId, size: 120 },
                { id: "orgAdmin", header: "Org admin", accessorFn: (entry) => entry.member?.orgAdmin, size: 100 },
                { id: "role", header: "Role", size: 130 },
                { id: "admin", header: "Administration", size: 200 },
                {
                  id: "memberships",
                  header: "Memberships",
                  accessorFn: (entry) => (entry.member?.environments ?? []).join(", "),
                  size: 420,
                },
              ]}
              formats={{ principalId: "short-hash", orgAdmin: "boolean-symbol" }}
              cells={{
                project: (cell) => <h2>{cell.row.project}</h2>,
                role: (cell) => <>{cell.row.member?.projectAdmin ? "Project admin" : "User"}</>,
                admin: (cell) => (
                  <Show
                    when={props.holds(
                      cell.row.member?.projectAdmin
                        ? projectAdmin.PROJECT_ADMIN_REVOKE_ROUTE.operation
                        : projectAdmin.PROJECT_ADMIN_GRANT_ROUTE.operation,
                      cell.row.project,
                    )}
                  >
                    <Button
                      disabled={pending() || form.pending}
                      onClick={() =>
                        void write(() =>
                          (cell.row.member?.projectAdmin ? projectAdmin.revoke : projectAdmin.grant)(props.transport, [
                            {
                              requestId: crypto.randomUUID(),
                              value: { project: cell.row.project, principalId },
                            },
                          ]),
                        )
                      }
                    >
                      {cell.row.member?.projectAdmin ? "Revoke project admin" : "Grant project admin"}
                    </Button>
                  </Show>
                ),
              }}
              details={(cell) => (
                <Records
                  id="user-environments"
                  title={`Memberships in ${cell.row.project}`}
                  rows={(cell.row.member?.environments ?? []).map((name) => ({
                    name,
                    state: cell.row.environments.find((env) => env.name === name)?.state,
                  }))}
                  rowKey={(env) => env.name}
                  columns={[
                    { id: "env", header: "Environment", accessorKey: "name" },
                    { id: "state", header: "State", accessorKey: "state" },
                    { id: "revoke", header: "Action", size: 240 },
                  ]}
                  cells={{
                    revoke: (entry) => (
                      <Show when={props.holds(member.MEMBER_REVOKE_ROUTE.operation, cell.row.project)}>
                        <Button
                          disabled={pending() || form.pending}
                          onClick={() =>
                            void write(() =>
                              member.revoke(props.transport, [
                                {
                                  requestId: crypto.randomUUID(),
                                  value: { project: cell.row.project, env: entry.row.name, principalId },
                                },
                              ]),
                            )
                          }
                        >
                          Revoke membership in {entry.row.name}
                        </Button>
                      </Show>
                    ),
                  }}
                />
              )}
            />
          </>
        )}
      </Show>
    </Page>
  );
}

export const userSections: readonly ShellSection[] = [
  {
    label: "Users",
    screens: [{ path: "users", label: "Users", operation: user.USER_LIST_ROUTE.operation, component: Users }],
    routes: [{ path: "users/:principal", operation: user.USER_LIST_ROUTE.operation, component: User }],
  },
];
