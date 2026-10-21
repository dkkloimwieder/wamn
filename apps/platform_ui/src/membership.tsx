import { createSignal, onCleanup, Show, untrack } from "solid-js";
import { FieldError } from "@wfe/components/fields";
import { createForm } from "@wfe/form";
import { FormView, type FormDescription } from "@wfe/generator";
import { environment, member, project } from "@wamn/control-org-client";
import { Page } from "./page";
import { value } from "./outcome";
import type { ScreenProps } from "./shell";

export function GrantMembership(props: ScreenProps & { returnTo: string }) {
  const defaults = untrack(() => ({
    principalId: props.params.principal ?? "",
    project: props.params.project ?? "",
    env: props.params.env ?? "",
  }));
  const [values, setValues] = createSignal(defaults);
  const [projects, setProjects] = createSignal<readonly { name: string; environments: readonly string[] }[]>([]);
  const [loaded, setLoaded] = createSignal(false);
  const [trouble, setTrouble] = createSignal<string>();
  let active = true;
  onCleanup(() => {
    active = false;
  });
  const load = async () => {
    const names = props.params.project
      ? [props.params.project]
      : value(await project.list(props.transport, [{}])).projects.map((entry) => entry.name);
    const choices = await Promise.all(
      names
        .filter((name) => props.holds(member.MEMBER_GRANT_ROUTE.operation, name))
        .map(async (name) => ({
          name,
          environments: value(await environment.list(props.transport, [{ project: name }])).environments.map(
            (env) => env.name,
          ),
        })),
    );
    if (active) {
      setProjects(choices);
      setLoaded(true);
    }
  };
  void untrack(load).catch((error: unknown) => {
    if (active) setTrouble(String(error));
  });
  const form = createForm({
    values,
    defaults: () => defaults,
    onValuesChange: (next) => setValues(next.project === values().project ? next : { ...next, env: "" }),
    validate: (snapshot) =>
      snapshot.principalId.trim() && snapshot.project && snapshot.env
        ? {}
        : { form: "Enter a user ID and select a project and an environment." },
    onSubmit: async (snapshot, context) => {
      try {
        value(await member.grant(props.transport, [{ requestId: crypto.randomUUID(), value: snapshot }]));
        if (!context.signal.aborted) props.open(props.returnTo);
        return { ok: true };
      } catch (error) {
        return { ok: false, errors: { form: String(error) } };
      }
    },
  });
  return (
    <Page title="Grant membership">
      <Show when={trouble()}>{(text) => <FieldError role="alert">{text()}</FieldError>}</Show>
      <Show when={loaded()}>
        <FormView
          description={
            {
              id: "grant-membership",
              fields: [
                ...(!props.params.principal
                  ? [{ field: "principalId", label: "User ID", control: "text" } as const]
                  : []),
                ...(!props.params.project ? [{ field: "project", label: "Project", control: "select" } as const] : []),
                ...(!props.params.env ? [{ field: "env", label: "Environment", control: "select" } as const] : []),
              ],
              submit: { id: "grant", label: "Grant membership" },
              actions: [{ id: "cancel", label: "Cancel", variant: "ghost" }],
            } as const satisfies FormDescription<typeof defaults>
          }
          bindings={{
            form,
            choices: {
              project: () => projects().map((entry) => ({ value: entry.name, label: entry.name })),
              env: () =>
                (projects().find((entry) => entry.name === form.values.project)?.environments ?? []).map((env) => ({
                  value: env,
                  label: env,
                })),
            },
            actions: { cancel: { run: () => props.open(props.returnTo) } },
          }}
        />
      </Show>
    </Page>
  );
}
