import { createSignal, For, onCleanup, Show, untrack } from "solid-js";
import { FieldError } from "@wfe/components/fields";
import { environment, project } from "@wamn/control-org-client";
import { Page, RecordLink } from "./page";
import { value } from "./outcome";
import { EnvironmentTiles } from "./status-tiles";
import type { ScreenProps } from "./shell";

export function ControlHome(props: ScreenProps) {
  const [projects, setProjects] = createSignal<string[]>(
    untrack(() =>
      props.controlMine?.orgAdmin
        ? []
        : (props.controlMine?.projects.filter((entry) => entry.projectAdmin).map((entry) => entry.project) ?? []),
    ),
  );
  const [trouble, setTrouble] = createSignal<string>();
  let active = true;
  onCleanup(() => {
    active = false;
  });
  const load = async () => {
    const names = value(await project.list(props.transport, [{}])).projects.map((entry) => entry.name);
    if (active) setProjects(names);
  };
  if (untrack(() => props.controlMine?.orgAdmin))
    void untrack(load).catch((error: unknown) => {
      if (active) setTrouble(String(error));
    });
  return (
    <Page title="Control">
      <Show when={trouble()}>{(message) => <FieldError role="alert">{message()}</FieldError>}</Show>
      <For each={projects()}>{(name) => <ProjectStatus screen={props} project={name} />}</For>
    </Page>
  );
}

function ProjectStatus(props: { screen: ScreenProps; project: string }) {
  const [environments, setEnvironments] = createSignal<readonly environment.EnvironmentListResultEnvironments[]>([]);
  const [trouble, setTrouble] = createSignal<string>();
  let active = true;
  onCleanup(() => {
    active = false;
  });
  void untrack(() => environment.list(props.screen.transport, [{ project: props.project }]))
    .then((result) => {
      if (active) setEnvironments(value(result).environments);
    })
    .catch((error: unknown) => {
      if (active) setTrouble(String(error));
    });
  return (
    <section aria-label={props.project}>
      <h2>{props.project}</h2>
      <Show when={trouble()}>{(message) => <FieldError role="alert">{message()}</FieldError>}</Show>
      <For each={environments()}>{(entry) => <EnvironmentStatus {...props} env={entry.name} />}</For>
    </section>
  );
}

function EnvironmentStatus(props: { screen: ScreenProps; project: string; env: string }) {
  const [status, setStatus] = createSignal<environment.EnvironmentStatusResult>();
  const [trouble, setTrouble] = createSignal<string>();
  let active = true;
  onCleanup(() => {
    active = false;
  });
  void untrack(() => environment.status(props.screen.transport, [{ project: props.project, env: props.env }]))
    .then((result) => {
      if (active) setStatus(value(result));
    })
    .catch((error: unknown) => {
      if (active) setTrouble(String(error));
    });
  return (
    <section aria-label={props.env}>
      <h3>
        <RecordLink
          screen={props.screen}
          path={`projects/${encodeURIComponent(props.project)}/${encodeURIComponent(props.env)}`}
        >
          {props.env}
        </RecordLink>
      </h3>
      <EnvironmentTiles status={status()} error={trouble()} />
    </section>
  );
}
