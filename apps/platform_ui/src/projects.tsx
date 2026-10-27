import { createEffect, createSignal, onCleanup, Show } from "solid-js";
import type { JSX } from "@solidjs/web";
import { Icon } from "@wfe/components";
import { Button, FieldError } from "@wfe/components/fields";
import { FormView, type FormDescription } from "@wfe/generator";
import { createServerPage, Records } from "./records";
import { createForm } from "@wfe/form";
import { control, environment, member, project, projectAdmin, release } from "./generated/wamn_control_org/index.js";
import type { Outcome } from "@wamn/web-runtime";
import { MissingPage, Page, RecordLink } from "./page";
import { GrantMembership } from "./membership";
import { value } from "./outcome";
import { ControlHome } from "./control-home";
import { Environment } from "./environment";
import type { ScreenProps, ShellSection } from "./shell";

export function Projects(props: ScreenProps): JSX.Element {
  const page = createServerPage<{ name: string; state?: string; projectAdmin?: boolean }>(async (input) => {
    if (!props.holds(project.PROJECT_LIST_ROUTE.operation)) {
      const mine = value(await control.mine(props.transport, [input]));
      return {
        ...mine,
        rows: mine.projects
          .filter((entry) => entry.projectAdmin)
          .map((entry) => ({ name: entry.project, projectAdmin: entry.projectAdmin })),
      };
    }
    const result = value(await project.list(props.transport, [input]));
    return {
      ...result,
      rows: result.projects.map((entry) => ({
        ...entry,
        projectAdmin: props.controlMine?.projects.find((held) => held.project === entry.name)?.projectAdmin,
      })),
    };
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
      <Show when={props.search.created === "1"}>
        <span role="status">Project created.</span>
      </Show>
      <Records
        refresh={refresh}
        paging={page}
        id="projects"
        title="Projects"
        create={
          props.holds(project.PROJECT_CREATE_ROUTE.operation)
            ? { label: "Create project", run: () => props.open("create-project") }
            : undefined
        }
        rows={page.rows()}
        rowKey={(entry) => entry.name}
        columns={[
          { id: "project", header: "Project" },
          ...(props.holds(project.PROJECT_LIST_ROUTE.operation)
            ? [{ id: "state", header: "State", accessorKey: "state" as const }]
            : []),
          { id: "projectAdmin", header: "Project admin", accessorKey: "projectAdmin" },
          { id: "orgAdmin", header: "Org admin", accessorFn: () => props.controlMine?.orgAdmin },
        ]}
        formats={{ projectAdmin: "boolean-symbol", orgAdmin: "boolean-symbol" }}
        cells={{
          project: (cell) => (
            <RecordLink screen={props} path={`projects/${encodeURIComponent(cell.row.name)}`}>
              {cell.row.name}
            </RecordLink>
          ),
        }}
      />
    </div>
  );
}

export function CreateProject(props: ScreenProps): JSX.Element {
  const defaults = { project: "" };
  const [values, setValues] = createSignal(defaults);
  const form = createForm({
    values,
    defaults: () => defaults,
    onValuesChange: setValues,
    validate: (snapshot) => (snapshot.project.trim() ? {} : { form: "Enter a project name." }),
    onSubmit: async (snapshot, context) => {
      try {
        value(await project.create(props.transport, [{ requestId: crypto.randomUUID(), value: snapshot }]));
        if (!context.signal.aborted) props.open("projects?created=1");
        return { ok: true };
      } catch (error) {
        return { ok: false, errors: { form: String(error) } };
      }
    },
  });
  return (
    <Page title="Create project">
      <FormView
        description={
          {
            id: "create-project",
            fields: [{ field: "project", label: "Project name", control: "text" }],
            submit: { id: "create-project", label: "Create project" },
            actions: [{ id: "cancel", label: "Cancel", variant: "ghost" }],
          } as const satisfies FormDescription<typeof defaults>
        }
        bindings={{ form, actions: { cancel: { run: () => props.open("projects") } } }}
      />
    </Page>
  );
}

export function Project(props: ScreenProps & { table?: "environments" | "members" }): JSX.Element {
  const name = () => props.params.project!;
  const projectBase = () => `projects/${encodeURIComponent(name())}`;
  const base = () => (props.params.env ? `${projectBase()}/${encodeURIComponent(props.params.env)}` : projectBase());
  const environmentPage = createServerPage<environment.EnvironmentListResultEnvironments>(async (input) => {
    const result = value(await environment.list(props.transport, [{ project: name(), ...input }]));
    return { ...result, rows: result.environments };
  });
  // The server keeps only the members of the selected environment.
  const memberScope = () => ({ project: name(), ...(props.params.env ? { environment: props.params.env } : {}) });
  const memberPage = createServerPage<member.MemberListResultMembers>(async (input) => {
    const result = value(await member.list(props.transport, [{ ...memberScope(), ...input }]));
    return { ...result, rows: result.members };
  });
  const [state, setState] = createSignal<"active" | "inactive">();
  const [members, setMembers] = createSignal<readonly member.MemberListResultMembers[]>([]);
  const [missing, setMissing] = createSignal(false);
  const [loaded, setLoaded] = createSignal(false);
  const [trouble, setTrouble] = createSignal<string>();
  const [pending, setPending] = createSignal(false);
  let active = true;
  let refreshGeneration = 0;
  onCleanup(() => {
    active = false;
  });
  const refresh = async () => {
    const current = ++refreshGeneration;
    setTrouble(undefined);
    if (props.holds(project.PROJECT_LIST_ROUTE.operation)) {
      const found = value(await project.list(props.transport, [{}])).projects.find((entry) => entry.name === name());
      if (!active || current !== refreshGeneration) return;
      setMissing(!found);
      if (!found) return;
      setState(found.state);
    }
    if (props.table === "environments") {
      // The table shows its own page error.
      await environmentPage.refresh().catch(() => {});
    } else if (props.table === "members") {
      // The environment lookup needs the complete set, even beyond page one.
      if (props.params.env) {
        const listed = value(await environment.list(props.transport, [{ project: name() }])).environments;
        if (!active || current !== refreshGeneration) return;
        if (!listed.some((env) => env.name === props.params.env)) {
          setMissing(true);
          return;
        }
      }
      // The table shows its own page error.
      await memberPage.refresh().catch(() => {});
    } else {
      const [envs, people] = await Promise.all([
        environment.list(props.transport, [{ project: name() }]),
        member.list(props.transport, [memberScope()]),
      ]);
      if (!active || current !== refreshGeneration) return;
      if ([envs, people].some((result) => result.status === "refused" && result.code === "project_not_found")) {
        setMissing(true);
        return;
      }
      const listed = value(envs).environments;
      if (props.params.env && !listed.some((env) => env.name === props.params.env)) {
        setMissing(true);
        return;
      }
      setMembers(value(people).members);
    }
    if (active && current === refreshGeneration) setLoaded(true);
  };
  createEffect(
    () => [props.transport, props.params.project, props.params.env, props.params.principal, props.table],
    () => {
      let current = true;
      environmentPage.reset();
      memberPage.reset();
      setMissing(false);
      setLoaded(false);
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
      if (active && !missing() && props.params.principal && !person()) props.open(`${base()}/members`);
    } catch (error) {
      if (active) setTrouble(String(error));
    } finally {
      if (active) setPending(false);
    }
  };
  const person = () => members().find((entry) => entry.principalId === props.params.principal);
  return (
    <Show when={!missing() && !(loaded() && props.params.principal && !person())} fallback={<MissingPage />}>
      <Show when={trouble()}>{(text) => <FieldError role="alert">{text()}</FieldError>}</Show>
      <Show when={!props.table && !props.params.principal}>
        <Page
          title={name()}
          actions={
            <Show
              when={
                state() &&
                props.holds(
                  state() === "active"
                    ? project.PROJECT_INACTIVATE_ROUTE.operation
                    : project.PROJECT_ACTIVATE_ROUTE.operation,
                )
              }
            >
              <Button
                disabled={pending()}
                onClick={() =>
                  void write(() =>
                    (state() === "active" ? project.inactivate : project.activate)(props.transport, [
                      { requestId: crypto.randomUUID(), value: { project: name() } },
                    ]),
                  )
                }
              >
                {state() === "active" ? "Inactivate project" : "Activate project"}
              </Button>
            </Show>
          }
        >
          <Show when={state()}>
            <p>State: {state()}</p>
          </Show>
        </Page>
      </Show>
      <Show when={props.table === "environments"}>
        <div class="platform-table-screen">
          <Records
            refresh={refresh}
            paging={environmentPage}
            id="environments"
            title="Environments"
            rows={environmentPage.rows()}
            rowKey={(env) => env.name}
            columns={[
              { id: "environment", header: "Environment" },
              { id: "state", header: "State", accessorKey: "state" },
            ]}
            cells={{
              environment: (cell) => (
                <RecordLink screen={props} path={`${base()}/${encodeURIComponent(cell.row.name)}`}>
                  {cell.row.name}
                </RecordLink>
              ),
            }}
          />
        </div>
      </Show>
      <Show when={props.table === "members"}>
        <div class="platform-table-screen">
          <Records
            refresh={refresh}
            paging={memberPage}
            id="members"
            title="Members"
            rows={memberPage.rows()}
            rowKey={(entry) => entry.principalId}
            create={
              props.holds(member.MEMBER_GRANT_ROUTE.operation)
                ? { label: "Grant membership", run: () => props.open(`${base()}/members/grant`) }
                : undefined
            }
            columns={[
              { id: "displayName", header: "Display name", accessorKey: "displayName", size: 180 },
              { id: "email", header: "Email", accessorKey: "email", size: 250 },
              { id: "principalId", header: "User ID", accessorKey: "principalId", size: 120 },
              { id: "orgAdmin", header: "Org admin", accessorKey: "orgAdmin", size: 100 },
              { id: "projectAdmin", header: "Project admin", accessorKey: "projectAdmin", size: 120 },
              {
                id: "memberships",
                header: "Memberships",
                accessorFn: (entry) => entry.environments.length,
              },
            ]}
            formats={{ principalId: "short-hash", orgAdmin: "boolean-symbol", projectAdmin: "boolean-symbol" }}
            cells={{
              displayName: (cell) => (
                <RecordLink screen={props} path={`${base()}/members/${encodeURIComponent(cell.row.principalId)}`}>
                  {cell.row.displayName}
                </RecordLink>
              ),
            }}
          />
        </div>
      </Show>
      <Show when={!!props.params.principal && person()}>
        {(current) => (
          <>
            <Page
              title={current().displayName}
              actions={
                <Show
                  when={props.holds(
                    current().projectAdmin
                      ? projectAdmin.PROJECT_ADMIN_REVOKE_ROUTE.operation
                      : projectAdmin.PROJECT_ADMIN_GRANT_ROUTE.operation,
                  )}
                >
                  <Button
                    disabled={pending()}
                    onClick={() =>
                      void write(() =>
                        (current().projectAdmin ? projectAdmin.revoke : projectAdmin.grant)(props.transport, [
                          {
                            requestId: crypto.randomUUID(),
                            value: { project: name(), principalId: current().principalId },
                          },
                        ]),
                      )
                    }
                  >
                    {current().projectAdmin ? "Revoke project admin" : "Grant project admin"}
                  </Button>
                </Show>
              }
            />
            <div class="platform-table-screen">
              <Records
                refresh={refresh}
                id="member-environments"
                title={`Memberships for ${current().email}`}
                rows={current().environments}
                rowKey={(env) => env}
                create={
                  props.holds(member.MEMBER_GRANT_ROUTE.operation)
                    ? {
                        label: "Grant membership",
                        run: () => props.open(`${base()}/members/${encodeURIComponent(current().principalId)}/grant`),
                      }
                    : undefined
                }
                columns={[
                  { id: "env", header: "Environment" },
                  { id: "revoke", header: "Action", size: 80, sortable: false, filterable: false },
                ]}
                cells={{
                  env: (cell) => (
                    <RecordLink screen={props} path={`${projectBase}/${encodeURIComponent(cell.row)}`}>
                      {cell.row}
                    </RecordLink>
                  ),
                  revoke: (cell) => (
                    <Show when={props.holds(member.MEMBER_REVOKE_ROUTE.operation)}>
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
                                value: { project: name(), env: cell.row, principalId: current().principalId },
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
          </>
        )}
      </Show>
    </Show>
  );
}

export const controlSections: readonly ShellSection[] = [
  {
    label: "Projects",
    screens: [
      {
        path: "projects",
        label: "Projects",
        operation: control.CONTROL_MINE_ROUTE.operation,
        component: Projects,
        scroll: "content",
      },
    ],
    routes: [
      { path: "", operation: control.CONTROL_MINE_ROUTE.operation, component: ControlHome },
      {
        path: "projects/:project/environments",
        label: "Environments",
        operation: environment.ENVIRONMENT_LIST_ROUTE.operation,
        component: (props) => <Project {...props} table="environments" />,
        scroll: "content",
      },
      {
        path: "projects/:project/members",
        label: "Members",
        operation: member.MEMBER_LIST_ROUTE.operation,
        component: (props) => <Project {...props} table="members" />,
        scroll: "content",
      },
      {
        path: "projects/:project/members/grant",
        label: "Grant membership",
        operation: member.MEMBER_GRANT_ROUTE.operation,
        component: (props) => (
          <GrantMembership {...props} returnTo={`projects/${encodeURIComponent(props.params.project!)}/members`} />
        ),
      },
      {
        path: "projects/:project/members/:principal/grant",
        label: "Grant membership",
        operation: member.MEMBER_GRANT_ROUTE.operation,
        component: (props) => (
          <GrantMembership
            {...props}
            returnTo={`projects/${encodeURIComponent(props.params.project!)}/members/${encodeURIComponent(props.params.principal!)}`}
          />
        ),
      },
      {
        path: "create-project",
        label: "Create project",
        operation: project.PROJECT_CREATE_ROUTE.operation,
        component: CreateProject,
      },
      {
        path: "projects/:project/members/:principal",
        operation: member.MEMBER_LIST_ROUTE.operation,
        component: Project,
        scroll: "content",
      },
      {
        path: "projects/:project",
        operation: member.MEMBER_LIST_ROUTE.operation,
        component: Project,
      },
      {
        path: "projects/:project/:env/workloads",
        label: "Workloads",
        operation: environment.ENVIRONMENT_STATUS_ROUTE.operation,
        component: (props) => <Environment {...props} table="workloads" />,
        scroll: "content",
      },
      {
        path: "projects/:project/:env/live-releases",
        label: "Live releases",
        operation: environment.ENVIRONMENT_STATUS_ROUTE.operation,
        component: (props) => <Environment {...props} table="live-releases" />,
        scroll: "content",
      },
      {
        path: "projects/:project/:env/operations",
        label: "Operations",
        operation: release.RELEASE_PROFILE_ROUTE.operation,
        component: (props) => <Environment {...props} table="operations" />,
        scroll: "content",
      },
      {
        path: "projects/:project/:env/roles",
        label: "Roles",
        operation: release.RELEASE_PROFILE_ROUTE.operation,
        component: (props) => <Environment {...props} table="roles" />,
        scroll: "content",
      },
      {
        path: "projects/:project/:env/members",
        label: "Members",
        operation: member.MEMBER_LIST_ROUTE.operation,
        component: (props) => <Project {...props} table="members" />,
        scroll: "content",
      },
      {
        path: "projects/:project/:env/members/grant",
        label: "Grant membership",
        operation: member.MEMBER_GRANT_ROUTE.operation,
        component: (props) => (
          <GrantMembership
            {...props}
            returnTo={`projects/${encodeURIComponent(props.params.project!)}/${encodeURIComponent(props.params.env!)}/members`}
          />
        ),
      },
      {
        path: "projects/:project/:env/members/:principal/grant",
        label: "Grant membership",
        operation: member.MEMBER_GRANT_ROUTE.operation,
        component: (props) => (
          <GrantMembership
            {...props}
            returnTo={`projects/${encodeURIComponent(props.params.project!)}/${encodeURIComponent(props.params.env!)}/members/${encodeURIComponent(props.params.principal!)}`}
          />
        ),
      },
      {
        path: "projects/:project/:env/members/:principal",
        operation: member.MEMBER_LIST_ROUTE.operation,
        component: Project,
        scroll: "content",
      },
      {
        path: "projects/:project/:env",
        operation: environment.ENVIRONMENT_STATUS_ROUTE.operation,
        component: Environment,
      },
    ],
  },
];
