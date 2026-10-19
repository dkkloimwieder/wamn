import { createSignal, For, onCleanup, Show } from "solid-js";
import type { JSX } from "@solidjs/web";
import { Button, Field, FieldError, Input, Label } from "@wfe/components/fields";
import { SelectField } from "@wfe/components/select";
import { createForm } from "@wfe/form";
import { control, environment, member, project, projectAdmin } from "@wamn/control-org-client";
import type { Outcome } from "@wamn/web-runtime";
import { Page } from "./page";
import type { ScreenProps, ShellSection } from "./shell";

function value<T>(outcome: Outcome<T>): T {
  if (outcome.status === "completed") return outcome.value;
  if (outcome.status === "refused") throw new Error(outcome.text ?? outcome.code ?? "The operation was refused.");
  if (outcome.status === "uncertain") throw new Error(`The result is unknown: ${outcome.reason}`);
  throw new Error(`The operation completed in part: ${JSON.stringify(outcome.failedOutcome)}`);
}

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
      value(await project.create(props.transport, [{ requestId: crypto.randomUUID(), value: snapshot }]));
      await refresh();
      return { ok: true };
    },
  });
  return (
    <Page title="Projects">
      <Show when={trouble()}>{(text) => <FieldError role="alert">{text()}</FieldError>}</Show>
      <ul aria-label="Projects">
        <For each={projects()}>
          {(name) => (
            <li>
              <Button onClick={() => props.open(`projects/${encodeURIComponent(name)}`)}>{name}</Button>
            </li>
          )}
        </For>
      </ul>
      <Show when={props.holds(project.PROJECT_CREATE_ROUTE.operation)}>
        <form
          onSubmit={(event) => {
            event.preventDefault();
            void form.submit();
          }}
        >
          <Field>
            <Label for="project-name">Project name</Label>
            <Input
              id="project-name"
              value={form.values.project}
              onValueChange={(name) => form.setValue("project", name)}
            />
          </Field>
          <Show when={form.errors.form}>
            <FieldError role="alert">{form.errors.form}</FieldError>
          </Show>
          <Show when={form.phase === "error"}>
            <FieldError role="alert">{String(form.failure)}</FieldError>
          </Show>
          <Button type="submit" disabled={form.pending}>
            Create project
          </Button>
          <Show when={form.phase === "success"}>
            <p role="status">Project created.</p>
          </Show>
        </form>
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
      value(
        await member.grant(props.transport, [
          { requestId: crypto.randomUUID(), value: { ...snapshot, project: name } },
        ]),
      );
      await refresh();
      return { ok: true };
    },
  });
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
      <ul aria-label="Environments">
        <For each={environments()}>{(env) => <li>{env}</li>}</For>
      </ul>
      <h2>Members</h2>
      <Show when={props.holds(member.MEMBER_GRANT_ROUTE.operation)}>
        <form
          onSubmit={(event) => {
            event.preventDefault();
            void form.submit();
          }}
        >
          <Field>
            <Label for="member-id">User ID</Label>
            <Input
              id="member-id"
              value={form.values.principalId}
              onValueChange={(id) => form.setValue("principalId", id)}
            />
          </Field>
          <Field>
            <Label>Environment</Label>
            <SelectField
              label="Environment"
              choices={environments().map((env) => ({ value: env, label: env }))}
              value={form.values.env}
              onValueChange={(env) => form.setValue("env", env)}
            />
          </Field>
          <Show when={form.errors.form}>
            <FieldError role="alert">{form.errors.form}</FieldError>
          </Show>
          <Show when={form.phase === "error"}>
            <FieldError role="alert">{String(form.failure)}</FieldError>
          </Show>
          <Button type="submit" disabled={form.pending || pending()}>
            Grant membership
          </Button>
          <Show when={form.phase === "success"}>
            <p role="status">Membership granted.</p>
          </Show>
        </form>
      </Show>
      <ul aria-label="Members">
        <For each={members()}>
          {(person) => (
            <li aria-label={person.email}>
              <p>
                {person.displayName} ({person.email})
              </p>
              <p>{person.projectAdmin ? "Project admin" : "Member"}</p>
              <Show
                when={props.holds(
                  person.projectAdmin
                    ? projectAdmin.PROJECT_ADMIN_REVOKE_ROUTE.operation
                    : projectAdmin.PROJECT_ADMIN_GRANT_ROUTE.operation,
                )}
              >
                <Button
                  disabled={pending() || form.pending}
                  onClick={() =>
                    void write(() =>
                      (person.projectAdmin ? projectAdmin.revoke : projectAdmin.grant)(props.transport, [
                        {
                          requestId: crypto.randomUUID(),
                          value: { project: name, principalId: person.principalId },
                        },
                      ]),
                    )
                  }
                >
                  {person.projectAdmin ? "Revoke project admin" : "Grant project admin"}
                </Button>
              </Show>
              <ul aria-label={`Memberships for ${person.email}`}>
                <For each={person.environments}>
                  {(env) => (
                    <li>
                      {env}
                      <Show when={props.holds(member.MEMBER_REVOKE_ROUTE.operation)}>
                        <Button
                          disabled={pending() || form.pending}
                          onClick={() =>
                            void write(() =>
                              member.revoke(props.transport, [
                                {
                                  requestId: crypto.randomUUID(),
                                  value: { project: name, env, principalId: person.principalId },
                                },
                              ]),
                            )
                          }
                        >
                          Revoke membership in {env}
                        </Button>
                      </Show>
                    </li>
                  )}
                </For>
              </ul>
            </li>
          )}
        </For>
      </ul>
    </Page>
  );
}

export const controlSections: readonly ShellSection[] = [
  {
    label: "Projects",
    screens: [
      { path: "projects", label: "Projects", operation: control.CONTROL_MINE_ROUTE.operation, component: Projects },
    ],
    routes: [{ path: "projects/:project", operation: member.MEMBER_LIST_ROUTE.operation, component: Project }],
  },
];
