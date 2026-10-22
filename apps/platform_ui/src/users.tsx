import { createEffect, createSignal, onCleanup, Show } from "solid-js";
import type { JSX } from "@solidjs/web";
import { Button, FieldError, Input } from "@wfe/components/fields";
import { Field, FieldValue, FormGrid, Icon, Label } from "@wfe/components";
import { FormView, type CustomFieldProps, type FormDescription } from "@wfe/generator";
import { createServerPage, Records } from "./records";
import { emailError } from "./email";
import { createForm } from "@wfe/form";
import { environment, member, orgAdmin, project, projectAdmin, user } from "@wamn/control-org-client";
import type { Outcome } from "@wamn/web-runtime";
import { MissingPage, Page, RecordLink } from "./page";
import { GrantMembership } from "./membership";
import { value } from "./outcome";
import type { ScreenProps, ShellSection } from "./shell";

export function Users(props: ScreenProps): JSX.Element {
  const page = createServerPage<user.UserListResultUsers>(async (input) => {
    const result = value(await user.list(props.transport, [input]));
    return { ...result, rows: result.users };
  });
  const refresh = () => page.refresh();
  createEffect(
    () => props.transport,
    () => {
      void refresh().catch(() => {});
    },
  );
  return (
    <div class="platform-table-screen">
      <Show when={props.search.invitation === "invited" || props.search.invitation === "enrolled"}>
        <span role="status">
          {props.search.invitation === "invited" ? "User invited." : "User is already enrolled."}
        </span>
      </Show>
      <Records
        refresh={refresh}
        paging={page}
        id="users"
        title="Users"
        create={
          props.holds(user.USER_INVITE_ROUTE.operation)
            ? { label: "Invite user", run: () => props.open("invite-user") }
            : undefined
        }
        rows={page.rows()}
        rowKey={(person) => person.principalId}
        columns={[
          { id: "name", header: "Display name", size: 180 },
          { id: "email", header: "Email", accessorKey: "email", size: 250 },
          { id: "principalId", header: "User ID", accessorKey: "principalId", size: 120 },
          { id: "state", header: "State", accessorKey: "status", size: 170 },
          { id: "orgAdmin", header: "Org admin", accessorKey: "orgAdmin", size: 120 },
        ]}
        formats={{ principalId: "short-hash", orgAdmin: "boolean-symbol" }}
        cells={{
          name: (cell) => (
            <RecordLink screen={props} path={`users/${encodeURIComponent(cell.row.principalId)}`}>
              {cell.row.displayName}
            </RecordLink>
          ),
        }}
      />
    </div>
  );
}

export function InviteUser(props: ScreenProps): JSX.Element {
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
    onSubmit: async (snapshot, context) => {
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
        if (!context.signal.aborted) props.open(`users?invitation=${invited.invited ? "invited" : "enrolled"}`);
        return { ok: true };
      } catch (error) {
        return { ok: false, errors: { form: String(error) } };
      }
    },
  });
  return (
    <Page title="Invite user">
      <FormView
        description={
          {
            id: "invite-user",
            fields: [
              { field: "email", label: "Email", control: "custom" },
              { field: "displayName", label: "Display name", control: "text" },
            ],
            submit: { id: "invite-user", label: "Invite user" },
            actions: [{ id: "cancel", label: "Cancel", variant: "ghost" }],
          } as const satisfies FormDescription<typeof defaults>
        }
        bindings={{
          form,
          fields: { email: Email },
          actions: { cancel: { run: () => props.open("users") } },
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

export function User(props: ScreenProps & { table?: "projects" | "memberships" }): JSX.Element {
  const principalId = () => props.params.principal!;
  const base = () => `users/${encodeURIComponent(principalId())}`;
  const [person, setPerson] = createSignal<user.UserListResultUsers>();
  const [projects, setProjects] = createSignal<readonly ProjectAccess[]>([]);
  const [missing, setMissing] = createSignal(false);
  const [trouble, setTrouble] = createSignal<string>();
  const [pending, setPending] = createSignal(false);
  let active = true;
  let refreshGeneration = 0;
  onCleanup(() => {
    active = false;
  });
  const readAccess = async (listed: readonly project.ProjectListResultProjects[]) => {
    return Promise.all(
      listed
        .filter((entry) => !props.params.project || entry.name === props.params.project)
        .map(async ({ name, state }) => {
          const [members, environments] = await Promise.all([
            member.list(props.transport, [{ project: name }]),
            environment.list(props.transport, [{ project: name }]),
          ]);
          return {
            project: name,
            state,
            environments: value(environments).environments,
            member: value(members).members.find((entry) => entry.principalId === principalId()),
          };
        }),
    );
  };
  const projectPage = createServerPage<ProjectAccess>(async (input) => {
    const result = value(await project.list(props.transport, [input]));
    return { ...result, rows: await readAccess(result.projects) };
  });
  const refresh = async () => {
    const current = ++refreshGeneration;
    setTrouble(undefined);
    const found = value(await user.list(props.transport, [{}])).users.find(
      (entry) => entry.principalId === principalId(),
    );
    if (!active || current !== refreshGeneration) return;
    setMissing(!found);
    if (!found) return;
    setPerson(found);
    if (!props.table) return;
    if (props.table === "projects") {
      await projectPage.refresh();
      return;
    }
    const listed = value(await project.list(props.transport, [{}])).projects;
    if (props.params.project && !listed.some((entry) => entry.name === props.params.project)) {
      setMissing(true);
      return;
    }
    const access = await readAccess(listed);
    if (active && current === refreshGeneration) setProjects(access);
  };
  createEffect(
    () => [props.transport, props.params.principal, props.params.project, props.table],
    () => {
      let current = true;
      projectPage.reset();
      void refresh().catch((error: unknown) => {
        if (active && current) setTrouble(String(error));
      });
      return () => {
        current = false;
        refreshGeneration++;
      };
    },
  );
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
  const access = () => projects().find((entry) => entry.project === props.params.project);
  const membershipPath = () =>
    props.params.project ? `${base()}/projects/${encodeURIComponent(props.params.project)}` : `${base()}/memberships`;
  return (
    <Show when={!missing()} fallback={<MissingPage />}>
      <Show when={trouble()}>{(text) => <FieldError role="alert">{text()}</FieldError>}</Show>
      <Show when={person()}>
        {(current) => (
          <>
            <Show when={!props.table}>
              <Page
                title={current().displayName}
                actions={
                  <>
                    <Show
                      when={props.holds(
                        current().status === "active"
                          ? user.USER_DEACTIVATE_ROUTE.operation
                          : user.USER_ACTIVATE_ROUTE.operation,
                      )}
                    >
                      <Button
                        disabled={pending()}
                        onClick={() =>
                          void write(() =>
                            (current().status === "active" ? user.deactivate : user.activate)(props.transport, [
                              { requestId: crypto.randomUUID(), value: { principalId: principalId() } },
                            ]),
                          )
                        }
                      >
                        {current().status === "active" ? "Inactivate user" : "Activate user"}
                      </Button>
                    </Show>
                    <Show
                      when={props.holds(
                        current().orgAdmin
                          ? orgAdmin.ORG_ADMIN_REVOKE_ROUTE.operation
                          : orgAdmin.ORG_ADMIN_GRANT_ROUTE.operation,
                      )}
                    >
                      <Button
                        disabled={pending()}
                        onClick={() =>
                          void write(() =>
                            (current().orgAdmin ? orgAdmin.revoke : orgAdmin.grant)(props.transport, [
                              { requestId: crypto.randomUUID(), value: { principalId: principalId() } },
                            ]),
                          )
                        }
                      >
                        {current().orgAdmin ? "Revoke org admin" : "Grant org admin"}
                      </Button>
                    </Show>
                  </>
                }
              >
                <p>{current().email}</p>
                <p>State: {current().status}</p>
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
              </Page>
            </Show>
            <Show when={props.table === "projects"}>
              <div class="platform-table-screen">
                <Records
                  refresh={refresh}
                  paging={projectPage}
                  id="project-access"
                  title="Project access"
                  rows={projectPage.rows()}
                  rowKey={(entry) => entry.project}
                  columns={[
                    { id: "project", header: "Project", accessorKey: "project" },
                    { id: "state", header: "State", accessorKey: "state" },
                    {
                      id: "displayName",
                      header: "Display name",
                      accessorFn: (entry) => entry.member?.displayName,
                      size: 180,
                    },
                    { id: "email", header: "Email", accessorFn: (entry) => entry.member?.email, size: 250 },
                    {
                      id: "principalId",
                      header: "User ID",
                      accessorFn: (entry) => entry.member?.principalId,
                      size: 120,
                    },
                    { id: "orgAdmin", header: "Org admin", accessorFn: (entry) => entry.member?.orgAdmin, size: 100 },
                    {
                      id: "projectAdmin",
                      header: "Project admin",
                      accessorFn: (entry) => entry.member?.projectAdmin ?? false,
                    },
                    {
                      id: "memberships",
                      header: "Memberships",
                      accessorFn: (entry) => entry.member?.environments.length ?? 0,
                    },
                  ]}
                  formats={{ principalId: "short-hash", orgAdmin: "boolean-symbol", projectAdmin: "boolean-symbol" }}
                  cells={{
                    project: (cell) => (
                      <RecordLink screen={props} path={`${base()}/projects/${encodeURIComponent(cell.row.project)}`}>
                        {cell.row.project}
                      </RecordLink>
                    ),
                  }}
                />
              </div>
            </Show>
            <Show when={props.table === "memberships"}>
              <Show when={access()}>
                {(entry) => (
                  <Page
                    title={entry().project}
                    actions={
                      <Show
                        when={props.holds(
                          entry().member?.projectAdmin
                            ? projectAdmin.PROJECT_ADMIN_REVOKE_ROUTE.operation
                            : projectAdmin.PROJECT_ADMIN_GRANT_ROUTE.operation,
                          entry().project,
                        )}
                      >
                        <Button
                          disabled={pending()}
                          onClick={() =>
                            void write(() =>
                              (entry().member?.projectAdmin ? projectAdmin.revoke : projectAdmin.grant)(
                                props.transport,
                                [
                                  {
                                    requestId: crypto.randomUUID(),
                                    value: { project: entry().project, principalId: principalId() },
                                  },
                                ],
                              ),
                            )
                          }
                        >
                          {entry().member?.projectAdmin ? "Revoke project admin" : "Grant project admin"}
                        </Button>
                      </Show>
                    }
                  />
                )}
              </Show>
              <div class="platform-table-screen">
                <Records
                  refresh={refresh}
                  id="user-environments"
                  title="Memberships"
                  create={
                    props.holds(member.MEMBER_GRANT_ROUTE.operation, props.params.project)
                      ? { label: "Grant membership", run: () => props.open(`${membershipPath()}/grant`) }
                      : undefined
                  }
                  rows={projects().flatMap((entry) =>
                    (entry.member?.environments ?? []).map((name) => ({
                      project: entry.project,
                      name,
                      state: entry.environments.find((env) => env.name === name)?.state,
                    })),
                  )}
                  rowKey={(env) => JSON.stringify([env.project, env.name])}
                  columns={[
                    { id: "project", header: "Project" },
                    { id: "env", header: "Environment" },
                    { id: "state", header: "State", accessorKey: "state" },
                    { id: "revoke", header: "Action", size: 80, sortable: false, filterable: false },
                  ]}
                  cells={{
                    project: (cell) => (
                      <RecordLink screen={props} path={`projects/${encodeURIComponent(cell.row.project)}`}>
                        {cell.row.project}
                      </RecordLink>
                    ),
                    env: (cell) => (
                      <RecordLink
                        screen={props}
                        path={`projects/${encodeURIComponent(cell.row.project)}/${encodeURIComponent(cell.row.name)}`}
                      >
                        {cell.row.name}
                      </RecordLink>
                    ),
                    revoke: (cell) => (
                      <Show when={props.holds(member.MEMBER_REVOKE_ROUTE.operation, cell.row.project)}>
                        <Button
                          class="ui-icon-button"
                          aria-label="Revoke membership"
                          title="Revoke membership"
                          disabled={pending()}
                          onClick={() =>
                            void write(() =>
                              member.revoke(props.transport, [
                                {
                                  requestId: crypto.randomUUID(),
                                  value: { project: cell.row.project, env: cell.row.name, principalId: principalId() },
                                },
                              ]),
                            )
                          }
                        >
                          <Icon name="cross" />
                        </Button>
                      </Show>
                    ),
                  }}
                />
              </div>
            </Show>
          </>
        )}
      </Show>
    </Show>
  );
}

export const userSections: readonly ShellSection[] = [
  {
    label: "Users",
    screens: [
      { path: "users", label: "Users", operation: user.USER_LIST_ROUTE.operation, component: Users, scroll: "content" },
    ],
    routes: [
      { path: "invite-user", label: "Invite user", operation: user.USER_INVITE_ROUTE.operation, component: InviteUser },
      { path: "users/:principal", operation: user.USER_LIST_ROUTE.operation, component: User },
      {
        path: "users/:principal/projects",
        label: "Project access",
        operation: user.USER_LIST_ROUTE.operation,
        component: (props) => <User {...props} table="projects" />,
        scroll: "content",
      },
      {
        path: "users/:principal/memberships",
        label: "Memberships",
        operation: user.USER_LIST_ROUTE.operation,
        component: (props) => <User {...props} table="memberships" />,
        scroll: "content",
      },
      {
        path: "users/:principal/projects/:project",
        operation: user.USER_LIST_ROUTE.operation,
        component: (props) => <User {...props} table="memberships" />,
        scroll: "content",
      },
      {
        path: "users/:principal/memberships/grant",
        label: "Grant membership",
        operation: user.USER_LIST_ROUTE.operation,
        component: (props) => (
          <GrantMembership {...props} returnTo={`users/${encodeURIComponent(props.params.principal!)}/memberships`} />
        ),
      },
      {
        path: "users/:principal/projects/:project/grant",
        label: "Grant membership",
        operation: user.USER_LIST_ROUTE.operation,
        component: (props) => (
          <GrantMembership
            {...props}
            returnTo={`users/${encodeURIComponent(props.params.principal!)}/projects/${encodeURIComponent(props.params.project!)}`}
          />
        ),
      },
    ],
  },
];
