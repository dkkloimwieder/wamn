import { createSignal, For, onCleanup, Show } from "solid-js";
import type { JSX } from "@solidjs/web";
import { Button, Field, FieldError, Input, Label } from "@wfe/components/fields";
import { SelectField } from "@wfe/components/select";
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
  const [values, setValues] = createSignal(defaults);
  const form = createForm({
    values,
    defaults: () => defaults,
    onValuesChange: setValues,
    validate: (snapshot) =>
      snapshot.email.trim() && snapshot.displayName.trim()
        ? {}
        : { form: "Enter an email address and a display name." },
    onSubmit: async (snapshot) => {
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
    },
  });
  return (
    <Page title="Users">
      <Show when={trouble()}>{(text) => <FieldError role="alert">{text()}</FieldError>}</Show>
      <Show when={props.holds(user.USER_INVITE_ROUTE.operation)}>
        <form
          onSubmit={(event) => {
            event.preventDefault();
            void form.submit();
          }}
        >
          <Field>
            <Label for="invite-email">Email</Label>
            <Input
              id="invite-email"
              type="email"
              value={form.values.email}
              onValueChange={(email) => form.setValue("email", email)}
            />
          </Field>
          <Field>
            <Label for="invite-name">Display name</Label>
            <Input
              id="invite-name"
              value={form.values.displayName}
              onValueChange={(name) => form.setValue("displayName", name)}
            />
          </Field>
          <Show when={form.errors.form}>
            <FieldError role="alert">{form.errors.form}</FieldError>
          </Show>
          <Show when={form.phase === "error"}>
            <FieldError role="alert">{String(form.failure)}</FieldError>
          </Show>
          <Button type="submit" disabled={pending() || form.pending}>
            Invite user
          </Button>
          <Show when={form.phase === "success"}>
            <p role="status">{invitation()?.invited ? "User invited." : "User is already enrolled."}</p>
          </Show>
        </form>
      </Show>
      <ul aria-label="Users">
        <For each={users()}>
          {(person) => (
            <li aria-label={person.email}>
              <Button onClick={() => props.open(`users/${encodeURIComponent(person.principalId)}`)}>
                {person.displayName}
              </Button>
              <p>{person.email}</p>
              <p>User state: {person.status}</p>
              <p>{person.orgAdmin ? "Org admin" : "User"}</p>
              <Show
                when={props.holds(
                  person.status === "active"
                    ? user.USER_DEACTIVATE_ROUTE.operation
                    : user.USER_ACTIVATE_ROUTE.operation,
                )}
              >
                <Button
                  disabled={pending() || form.pending}
                  onClick={() =>
                    void write(() =>
                      (person.status === "active" ? user.deactivate : user.activate)(props.transport, [
                        { requestId: crypto.randomUUID(), value: { principalId: person.principalId } },
                      ]),
                    )
                  }
                >
                  {person.status === "active" ? "Deactivate user" : "Activate user"}
                </Button>
              </Show>
              <Show
                when={props.holds(
                  person.orgAdmin
                    ? orgAdmin.ORG_ADMIN_REVOKE_ROUTE.operation
                    : orgAdmin.ORG_ADMIN_GRANT_ROUTE.operation,
                )}
              >
                <Button
                  disabled={pending() || form.pending}
                  onClick={() =>
                    void write(() =>
                      (person.orgAdmin ? orgAdmin.revoke : orgAdmin.grant)(props.transport, [
                        { requestId: crypto.randomUUID(), value: { principalId: person.principalId } },
                      ]),
                    )
                  }
                >
                  {person.orgAdmin ? "Revoke org admin" : "Grant org admin"}
                </Button>
              </Show>
            </li>
          )}
        </For>
      </ul>
    </Page>
  );
}

interface ProjectAccess {
  readonly project: string;
  readonly environments: readonly string[];
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
      value(listed).projects.map(async ({ name }) => {
        const [members, environments] = await Promise.all([
          member.list(props.transport, [{ project: name }]),
          environment.list(props.transport, [{ project: name }]),
        ]);
        return {
          project: name,
          environments: value(environments).environments.map((entry) => entry.name),
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
    onValuesChange: setValues,
    validate: (snapshot) => (snapshot.project && snapshot.env ? {} : { form: "Select a project and an environment." }),
    onSubmit: async (snapshot) => {
      value(
        await member.grant(props.transport, [{ requestId: crypto.randomUUID(), value: { ...snapshot, principalId } }]),
      );
      await refresh();
      return { ok: true };
    },
  });
  return (
    <Page title={person()?.displayName ?? "User"}>
      <Show when={trouble()}>{(text) => <FieldError role="alert">{text()}</FieldError>}</Show>
      <Show when={person()}>
        {(current) => (
          <>
            <p>{current().email}</p>
            <p>User state: {current().status}</p>
            <Show when={props.holds(member.MEMBER_GRANT_ROUTE.operation, form.values.project || undefined)}>
              <form
                onSubmit={(event) => {
                  event.preventDefault();
                  void form.submit();
                }}
              >
                <Field>
                  <Label>Project</Label>
                  <SelectField
                    label="Project"
                    choices={projects().map((entry) => ({ value: entry.project, label: entry.project }))}
                    value={form.values.project}
                    onValueChange={(name) => setValues({ project: name, env: "" })}
                  />
                </Field>
                <Field>
                  <Label>Environment</Label>
                  <SelectField
                    label="Environment"
                    choices={(
                      projects().find((entry) => entry.project === form.values.project)?.environments ?? []
                    ).map((env) => ({ value: env, label: env }))}
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
                <Button type="submit" disabled={pending() || form.pending}>
                  Grant membership
                </Button>
                <Show when={form.phase === "success"}>
                  <p role="status">Membership granted.</p>
                </Show>
              </form>
            </Show>
            <ul aria-label="Project access">
              <For each={projects()}>
                {(entry) => (
                  <li aria-label={entry.project}>
                    <h2>{entry.project}</h2>
                    <p>{entry.member?.projectAdmin ? "Project admin" : "User"}</p>
                    <Show
                      when={props.holds(
                        entry.member?.projectAdmin
                          ? projectAdmin.PROJECT_ADMIN_REVOKE_ROUTE.operation
                          : projectAdmin.PROJECT_ADMIN_GRANT_ROUTE.operation,
                        entry.project,
                      )}
                    >
                      <Button
                        disabled={pending() || form.pending}
                        onClick={() =>
                          void write(() =>
                            (entry.member?.projectAdmin ? projectAdmin.revoke : projectAdmin.grant)(props.transport, [
                              { requestId: crypto.randomUUID(), value: { project: entry.project, principalId } },
                            ]),
                          )
                        }
                      >
                        {entry.member?.projectAdmin ? "Revoke project admin" : "Grant project admin"}
                      </Button>
                    </Show>
                    <ul aria-label={`Memberships in ${entry.project}`}>
                      <For each={entry.member?.environments ?? []}>
                        {(env) => (
                          <li>
                            {env}
                            <Show when={props.holds(member.MEMBER_REVOKE_ROUTE.operation, entry.project)}>
                              <Button
                                disabled={pending() || form.pending}
                                onClick={() =>
                                  void write(() =>
                                    member.revoke(props.transport, [
                                      {
                                        requestId: crypto.randomUUID(),
                                        value: { project: entry.project, env, principalId },
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
