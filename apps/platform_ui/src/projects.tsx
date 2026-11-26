import { createEffect, createSignal, onCleanup, Show } from "solid-js";
import type { JSX } from "@solidjs/web";
import { Icon } from "@wfe/components";
import { Button, FieldError } from "@wfe/components/fields";
import { FormView, type FormDescription } from "@wfe/generator";
import { createServerPage, Records } from "./records";
import { createForm } from "@wfe/form";
import {
  control,
  controlFields,
  envRoutes,
  member,
  project,
  projectAdmin,
  type EnvListResultEnvs,
  type MemberListResultMembers,
} from "./control-client";
import type { Outcome } from "@wamn/web-runtime";
import { MissingPage, Page, RecordLink } from "./page";
import { GrantMembership } from "./membership";
import { value } from "./outcome";
import { ControlHome } from "./control-home";
import { Environment } from "./env";
import { envSegment } from "./address";
import type { ScreenProps, ShellSection } from "./shell";

function Projects(props: ScreenProps): JSX.Element {
  const page = createServerPage<{ name: string; state?: string; project_admin?: boolean }>(async (input) => {
    if (!props.holds(project.list.route.operation)) {
      const mine = value(await control.mine(props.transport, [input]));
      return {
        ...mine,
        rows: mine.projects
          .filter((entry) => entry.project_admin)
          .map((entry) => ({ name: entry.project, project_admin: entry.project_admin })),
      };
    }
    const result = value(await project.list(props.transport, [input]));
    return {
      ...result,
      rows: result.projects.map((entry) => ({
        ...entry,
        project_admin: props.controlMine?.projects.find((held) => held.project === entry.name)?.project_admin,
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
          props.holds(project.create.route.operation)
            ? { label: "Create project", run: () => props.open("create-project") }
            : undefined
        }
        rows={page.rows()}
        rowKey={(entry) => entry.name}
        columns={[
          { id: "project", header: controlFields.current.projects.items.properties.project.title },
          ...(props.holds(project.list.route.operation)
            ? [{ id: "state", header: controlFields.env.state.title, accessorKey: "state" as const }]
            : []),
          {
            id: "project_admin",
            header: controlFields.current.projects.items.properties.project_admin.title,
            accessorKey: "project_admin",
          },
          {
            id: "org_admin",
            header: controlFields.current.org_admin.title,
            accessorFn: () => props.controlMine?.org_admin,
          },
        ]}
        formats={{ project_admin: "boolean-symbol", org_admin: "boolean-symbol" }}
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

function CreateProject(props: ScreenProps): JSX.Element {
  const defaults = { project: "" };
  const [values, setValues] = createSignal(defaults);
  const form = createForm({
    values,
    defaults: () => defaults,
    onValuesChange: setValues,
    validate: (snapshot) => (snapshot.project.trim() ? {} : { form: "Enter a project name." }),
    onSubmit: async (snapshot, context) => {
      try {
        value(await project.create(props.transport, [{ request_id: crypto.randomUUID(), value: snapshot }]));
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
            fields: [{ field: "project", label: controlFields.createProject.project.title, control: "text" }],
            submit: { id: "create-project", label: "Create project" },
            actions: [{ id: "cancel", label: "Cancel", variant: "ghost" }],
          } as const satisfies FormDescription<typeof defaults>
        }
        bindings={{ form, actions: { cancel: { run: () => props.open("projects") } } }}
      />
    </Page>
  );
}

export function Project(props: ScreenProps & { table?: "envs" | "members" }): JSX.Element {
  const name = () => props.params.project!;
  const projectBase = () => `projects/${encodeURIComponent(name())}`;
  const base = () => (props.params.env ? `${projectBase()}/${envSegment(props.params.env)}` : projectBase());
  const envPage = createServerPage<EnvListResultEnvs>(async (input) => {
    const result = value(await envRoutes.list(props.transport, [{ project: name(), ...input }]));
    return { ...result, rows: result.envs };
  });
  // The server keeps only the members of the selected environment.
  const memberFilter = () => ({ project: name(), ...(props.params.env ? { env: props.params.env } : {}) });
  const memberPage = createServerPage<MemberListResultMembers>(async (input) => {
    const result = value(await member.list(props.transport, [{ ...memberFilter(), ...input }]));
    return { ...result, rows: result.members };
  });
  const [state, setState] = createSignal<"active" | "inactive">();
  const [members, setMembers] = createSignal<readonly MemberListResultMembers[]>([]);
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
    if (props.holds(project.list.route.operation)) {
      const found = value(await project.list(props.transport, [{}])).projects.find((entry) => entry.name === name());
      if (!active || current !== refreshGeneration) return;
      setMissing(!found);
      if (!found) return;
      setState(found.state);
    }
    if (props.table === "envs") {
      // The table shows its own page error.
      await envPage.refresh().catch(() => {});
    } else if (props.table === "members") {
      // The environment lookup needs the complete set, even beyond page one.
      if (props.params.env) {
        const listed = value(await envRoutes.list(props.transport, [{ project: name() }])).envs;
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
        envRoutes.list(props.transport, [{ project: name() }]),
        member.list(props.transport, [memberFilter()]),
      ]);
      if (!active || current !== refreshGeneration) return;
      if ([envs, people].some((result) => result.status === "refused" && result.code === "project_not_found")) {
        setMissing(true);
        return;
      }
      const listed = value(envs).envs;
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
      envPage.reset();
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
  const person = () => members().find((entry) => entry.principal_id === props.params.principal);
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
                  state() === "active" ? project.inactivate.route.operation : project.activate.route.operation,
                )
              }
            >
              <Button
                disabled={pending()}
                onClick={() =>
                  void write(() =>
                    (state() === "active" ? project.inactivate : project.activate)(props.transport, [
                      { request_id: crypto.randomUUID(), value: { project: name() } },
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
            <p>
              {controlFields.env.state.title}: {state()}
            </p>
          </Show>
        </Page>
      </Show>
      <Show when={props.table === "envs"}>
        <div class="platform-table-screen">
          <Records
            refresh={refresh}
            paging={envPage}
            id="envs"
            title="Environments"
            rows={envPage.rows()}
            rowKey={(env) => env.name}
            columns={[
              { id: "env", header: controlFields.grantMember.env.title },
              { id: "state", header: controlFields.env.state.title, accessorKey: "state" },
            ]}
            cells={{
              env: (cell) => (
                <RecordLink screen={props} path={`${base()}/${envSegment(cell.row.name)}`}>
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
            rowKey={(entry) => entry.principal_id}
            create={
              props.holds(member.grant.route.operation)
                ? { label: "Grant membership", run: () => props.open(`${base()}/members/grant`) }
                : undefined
            }
            columns={[
              {
                id: "display_name",
                header: controlFields.member.display_name.title,
                accessorKey: "display_name",
                size: 180,
              },
              { id: "email", header: controlFields.member.email.title, accessorKey: "email", size: 250 },
              {
                id: "principal_id",
                header: controlFields.member.principal_id.title,
                accessorKey: "principal_id",
                size: 120,
              },
              { id: "org_admin", header: controlFields.member.org_admin.title, accessorKey: "org_admin", size: 100 },
              {
                id: "project_admin",
                header: controlFields.member.project_admin.title,
                accessorKey: "project_admin",
                size: 120,
              },
              {
                id: "memberships",
                header: controlFields.member.envs.title,
                accessorFn: (entry) => entry.envs.length,
              },
            ]}
            formats={{ principal_id: "short-hash", org_admin: "boolean-symbol", project_admin: "boolean-symbol" }}
            cells={{
              display_name: (cell) => (
                <RecordLink screen={props} path={`${base()}/members/${encodeURIComponent(cell.row.principal_id)}`}>
                  {cell.row.display_name}
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
              title={current().display_name}
              actions={
                <Show
                  when={props.holds(
                    current().project_admin ? projectAdmin.revoke.route.operation : projectAdmin.grant.route.operation,
                  )}
                >
                  <Button
                    disabled={pending()}
                    onClick={() =>
                      void write(() =>
                        (current().project_admin ? projectAdmin.revoke : projectAdmin.grant)(props.transport, [
                          {
                            request_id: crypto.randomUUID(),
                            value: { project: name(), principal_id: current().principal_id },
                          },
                        ]),
                      )
                    }
                  >
                    {current().project_admin ? "Revoke project admin" : "Grant project admin"}
                  </Button>
                </Show>
              }
            />
            <div class="platform-table-screen">
              <Records
                refresh={refresh}
                id="member-envs"
                title={`Memberships for ${current().email}`}
                rows={current().envs}
                rowKey={(env) => env}
                create={
                  props.holds(member.grant.route.operation)
                    ? {
                        label: "Grant membership",
                        run: () => props.open(`${base()}/members/${encodeURIComponent(current().principal_id)}/grant`),
                      }
                    : undefined
                }
                columns={[
                  { id: "env", header: controlFields.grantMember.env.title },
                  { id: "revoke", header: "Action", size: 80, sortable: false, filterable: false },
                ]}
                cells={{
                  env: (cell) => (
                    <RecordLink screen={props} path={`${projectBase}/${encodeURIComponent(cell.row)}`}>
                      {cell.row}
                    </RecordLink>
                  ),
                  revoke: (cell) => (
                    <Show when={props.holds(member.revoke.route.operation)}>
                      <Button
                        class="ui-icon-button"
                        aria-label="Revoke membership"
                        title="Revoke membership"
                        disabled={pending()}
                        onClick={() =>
                          void write(() =>
                            member.revoke(props.transport, [
                              {
                                request_id: crypto.randomUUID(),
                                value: { project: name(), env: cell.row, principal_id: current().principal_id },
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
        operation: control.mine.route.operation,
        component: Projects,
        scroll: "content",
      },
    ],
    routes: [
      { path: "", operation: control.mine.route.operation, component: ControlHome },
      {
        path: "projects/:project/envs",
        label: "Environments",
        operation: envRoutes.list.route.operation,
        component: (props) => <Project {...props} table="envs" />,
        scroll: "content",
      },
      {
        path: "projects/:project/members",
        label: "Members",
        operation: member.list.route.operation,
        component: (props) => <Project {...props} table="members" />,
        scroll: "content",
      },
      {
        path: "projects/:project/members/grant",
        label: "Grant membership",
        operation: member.grant.route.operation,
        component: (props) => (
          <GrantMembership {...props} returnTo={`projects/${encodeURIComponent(props.params.project!)}/members`} />
        ),
      },
      {
        path: "projects/:project/members/:principal/grant",
        label: "Grant membership",
        operation: member.grant.route.operation,
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
        operation: project.create.route.operation,
        component: CreateProject,
      },
      {
        path: "projects/:project/members/:principal",
        operation: member.list.route.operation,
        component: Project,
        scroll: "content",
      },
      {
        path: "projects/:project",
        operation: member.list.route.operation,
        component: Project,
      },
      {
        path: "projects/:project/:env/workloads",
        label: "Workloads",
        operation: envRoutes.status.route.operation,
        component: (props) => <Environment {...props} table="workloads" />,
        scroll: "content",
      },
      {
        path: "projects/:project/:env/live-releases",
        label: "Live releases",
        operation: envRoutes.status.route.operation,
        component: (props) => <Environment {...props} table="live-releases" />,
        scroll: "content",
      },
      {
        path: "projects/:project/:env/members",
        label: "Members",
        operation: member.list.route.operation,
        component: (props) => <Project {...props} table="members" />,
        scroll: "content",
      },
      {
        path: "projects/:project/:env/members/grant",
        label: "Grant membership",
        operation: member.grant.route.operation,
        component: (props) => (
          <GrantMembership
            {...props}
            returnTo={`projects/${encodeURIComponent(props.params.project!)}/${envSegment(props.params.env!)}/members`}
          />
        ),
      },
      {
        path: "projects/:project/:env/members/:principal/grant",
        label: "Grant membership",
        operation: member.grant.route.operation,
        component: (props) => (
          <GrantMembership
            {...props}
            returnTo={`projects/${encodeURIComponent(props.params.project!)}/${envSegment(props.params.env!)}/members/${encodeURIComponent(props.params.principal!)}`}
          />
        ),
      },
      {
        path: "projects/:project/:env/members/:principal",
        operation: member.list.route.operation,
        component: Project,
        scroll: "content",
      },
      {
        path: "projects/:project/:env",
        operation: envRoutes.status.route.operation,
        component: Environment,
      },
    ],
  },
];
