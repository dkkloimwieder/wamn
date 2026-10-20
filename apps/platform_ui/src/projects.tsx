import { createSignal, onCleanup, Show } from "solid-js";
import type { JSX } from "@solidjs/web";
import { Button, FieldError } from "@wfe/components/fields";
import { FormView, type FormDescription } from "@wfe/generator";
import { Records } from "./records";
import { createForm } from "@wfe/form";
import { control, environment, member, project, projectAdmin } from "@wamn/control-org-client";
import type { Outcome } from "@wamn/web-runtime";
import { Page } from "./page";
import { value } from "./outcome";
import { Environment } from "./environment";
import type { ScreenProps, ShellSection } from "./shell";

export function Projects(props: ScreenProps): JSX.Element {
  const [projects, setProjects] = createSignal<readonly string[]>(props.controlProjects ?? []);
  const [trouble, setTrouble] = createSignal<string>();
  let active = true;
  onCleanup(() => {
    active = false;
  });
  const refresh = async () => {
    if (!props.holds(project.PROJECT_LIST_ROUTE.operation)) return;
    const result = value(await project.list(props.transport, [{}]));
    if (active) setProjects(result.projects.map((entry) => entry.name));
  };
  void refresh().catch((error: unknown) => {
    if (active) setTrouble(String(error));
  });
  const defaults = { project: "" };
  const [values, setValues] = createSignal(defaults);
  const form = createForm({
    values,
    defaults: () => defaults,
    onValuesChange: setValues,
    validate: (snapshot) => (snapshot.project.trim() ? {} : { form: "Enter a project name." }),
    onSubmit: async (snapshot) => {
      try {
        value(await project.create(props.transport, [{ requestId: crypto.randomUUID(), value: snapshot }]));
        await refresh();
        return { ok: true };
      } catch (error) {
        return { ok: false, errors: { form: String(error) } };
      }
    },
  });
  return (
    <Page title="Projects">
      <Show when={trouble()}>{(text) => <FieldError role="alert">{text()}</FieldError>}</Show>
      <Records
        id="projects"
        title="Projects"
        rows={projects()}
        rowKey={(name) => name}
        columns={[{ id: "project", header: "Project" }]}
        cells={{
          project: (cell) => (
            <Button onClick={() => props.open(`projects/${encodeURIComponent(cell.row)}`)}>{cell.row}</Button>
          ),
        }}
      />
      <Show when={props.holds(project.PROJECT_CREATE_ROUTE.operation)}>
        <FormView
          description={
            {
              id: "create-project",
              fields: [{ field: "project", label: "Project name", control: "text" }],
              submit: { id: "create-project", label: "Create project" },
            } as const satisfies FormDescription<typeof defaults>
          }
          bindings={{ form }}
        />
        <Show when={form.phase === "success"}>
          <p role="status">Project created.</p>
        </Show>
      </Show>
    </Page>
  );
}

export function Project(props: ScreenProps): JSX.Element {
  const name = props.params.project!;
  const [environments, setEnvironments] = createSignal<readonly string[]>([]);
  const [state, setState] = createSignal<"active" | "inactive">();
  const [members, setMembers] = createSignal<readonly member.MemberListResultMembers[]>([]);
  const [trouble, setTrouble] = createSignal<string>();
  const [pending, setPending] = createSignal(false);
  let active = true;
  onCleanup(() => {
    active = false;
  });
  const refresh = async () => {
    const [envs, people, projects] = await Promise.all([
      environment.list(props.transport, [{ project: name }]),
      member.list(props.transport, [{ project: name }]),
      props.holds(project.PROJECT_LIST_ROUTE.operation) ? project.list(props.transport, [{}]) : undefined,
    ]);
    const nextEnvironments = value(envs).environments;
    const nextMembers = value(people).members;
    if (active) {
      setEnvironments(nextEnvironments.map((entry) => entry.name));
      if (projects) setState(value(projects).projects.find((entry) => entry.name === name)?.state);
      setMembers(nextMembers);
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
  const defaults = { principalId: "", env: "" };
  const [values, setValues] = createSignal(defaults);
  const form = createForm({
    values,
    defaults: () => defaults,
    onValuesChange: setValues,
    validate: (snapshot) =>
      snapshot.principalId.trim() && snapshot.env ? {} : { form: "Enter a user ID and select an environment." },
    onSubmit: async (snapshot) => {
      try {
        value(
          await member.grant(props.transport, [
            { requestId: crypto.randomUUID(), value: { ...snapshot, project: name } },
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
    <Page title={name}>
      <Show when={trouble()}>{(text) => <FieldError role="alert">{text()}</FieldError>}</Show>
      <Show when={state()}>
        <p>Project state: {state()}</p>
        <Show
          when={props.holds(
            state() === "active"
              ? project.PROJECT_INACTIVATE_ROUTE.operation
              : project.PROJECT_ACTIVATE_ROUTE.operation,
          )}
        >
          <Button
            disabled={pending() || form.pending}
            onClick={() =>
              void write(() =>
                (state() === "active" ? project.inactivate : project.activate)(props.transport, [
                  { requestId: crypto.randomUUID(), value: { project: name } },
                ]),
              )
            }
          >
            {state() === "active" ? "Inactivate project" : "Activate project"}
          </Button>
        </Show>
      </Show>
      <h2>Environments</h2>
      <Records
        id="environments"
        title="Environments"
        rows={environments()}
        rowKey={(env) => env}
        columns={[{ id: "environment", header: "Environment" }]}
        cells={{
          environment: (cell) => (
            <Button onClick={() => props.open(`projects/${encodeURIComponent(name)}/${encodeURIComponent(cell.row)}`)}>
              {cell.row}
            </Button>
          ),
        }}
      />
      <h2>Members</h2>
      <Show when={props.holds(member.MEMBER_GRANT_ROUTE.operation)}>
        <FormView
          description={
            {
              id: "project-membership",
              fields: [
                { field: "principalId", label: "User ID", control: "text" },
                { field: "env", label: "Environment", control: "select" },
              ],
              submit: { id: "grant-membership", label: "Grant membership" },
            } as const satisfies FormDescription<typeof defaults>
          }
          bindings={{
            form: membershipForm,
            choices: { env: () => environments().map((env) => ({ value: env, label: env })) },
          }}
        />
        <Show when={form.phase === "success"}>
          <p role="status">Membership granted.</p>
        </Show>
      </Show>
      <Records
        id="members"
        title="Members"
        rows={members()}
        rowKey={(person) => person.principalId}
        columns={[
          { id: "person", header: "Person", size: 260 },
          { id: "role", header: "Role", size: 130 },
          { id: "admin", header: "Administration", size: 200 },
          { id: "memberships", header: "Memberships", size: 420 },
        ]}
        cells={{
          person: (cell) => (
            <>
              {cell.row.displayName} ({cell.row.email})
            </>
          ),
          role: (cell) => <>{cell.row.projectAdmin ? "Project admin" : "Member"}</>,
          admin: (cell) => (
            <Show
              when={props.holds(
                cell.row.projectAdmin
                  ? projectAdmin.PROJECT_ADMIN_REVOKE_ROUTE.operation
                  : projectAdmin.PROJECT_ADMIN_GRANT_ROUTE.operation,
              )}
            >
              <Button
                disabled={pending() || form.pending}
                onClick={() =>
                  void write(() =>
                    (cell.row.projectAdmin ? projectAdmin.revoke : projectAdmin.grant)(props.transport, [
                      {
                        requestId: crypto.randomUUID(),
                        value: { project: name, principalId: cell.row.principalId },
                      },
                    ]),
                  )
                }
              >
                {cell.row.projectAdmin ? "Revoke project admin" : "Grant project admin"}
              </Button>
            </Show>
          ),
          memberships: (cell) => (
            <Records
              id="member-environments"
              title={`Memberships for ${cell.row.email}`}
              rows={cell.row.environments}
              rowKey={(env) => env}
              columns={[
                { id: "env", header: "Environment" },
                { id: "revoke", header: "Action", size: 240 },
              ]}
              cells={{
                env: (entry) => <>{entry.row}</>,
                revoke: (entry) => (
                  <Show when={props.holds(member.MEMBER_REVOKE_ROUTE.operation)}>
                    <Button
                      disabled={pending() || form.pending}
                      onClick={() =>
                        void write(() =>
                          member.revoke(props.transport, [
                            {
                              requestId: crypto.randomUUID(),
                              value: {
                                project: name,
                                env: entry.row,
                                principalId: cell.row.principalId,
                              },
                            },
                          ]),
                        )
                      }
                    >
                      Revoke membership in {entry.row}
                    </Button>
                  </Show>
                ),
              }}
            />
          ),
        }}
      />
    </Page>
  );
}

export const controlSections: readonly ShellSection[] = [
  {
    label: "Projects",
    screens: [
      { path: "projects", label: "Projects", operation: control.CONTROL_MINE_ROUTE.operation, component: Projects },
    ],
    routes: [
      { path: "projects/:project", operation: member.MEMBER_LIST_ROUTE.operation, component: Project },
      {
        path: "projects/:project/:env",
        operation: environment.ENVIRONMENT_STATUS_ROUTE.operation,
        component: Environment,
      },
    ],
  },
];
