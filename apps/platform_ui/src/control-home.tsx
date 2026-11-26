import { createSignal, For, onCleanup, Show, untrack } from "solid-js";
import { FieldError } from "@wfe/components/fields";
import {
  envRoutes,
  project,
  type EnvListResultEnvs,
  type EnvStatusResult,
} from "./control-client";
import { Page, RecordLink } from "./page";
import { value } from "./outcome";
import { EnvironmentTiles } from "./status-tiles";
import type { ScreenProps } from "./shell";
import { envSegment } from "./address";

export function ControlHome(props: ScreenProps) {
  const [projects, setProjects] = createSignal<string[]>(
    untrack(() =>
      props.controlMine?.org_admin
        ? []
        : (props.controlMine?.projects.filter((entry) => entry.project_admin).map((entry) => entry.project) ?? []),
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
  if (untrack(() => props.controlMine?.org_admin))
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
  const [envs, setEnvs] = createSignal<readonly EnvListResultEnvs[]>([]);
  const [trouble, setTrouble] = createSignal<string>();
  let active = true;
  onCleanup(() => {
    active = false;
  });
  void untrack(() => envRoutes.list(props.screen.transport, [{ project: props.project }]))
    .then((result) => {
      if (active) setEnvs(value(result).envs);
    })
    .catch((error: unknown) => {
      if (active) setTrouble(String(error));
    });
  return (
    <section aria-label={props.project}>
      <h2>{props.project}</h2>
      <Show when={trouble()}>{(message) => <FieldError role="alert">{message()}</FieldError>}</Show>
      <For each={envs()}>{(entry) => <EnvironmentStatus {...props} env={entry.name} />}</For>
    </section>
  );
}

function EnvironmentStatus(props: { screen: ScreenProps; project: string; env: string }) {
  const [status, setStatus] = createSignal<EnvStatusResult>();
  const [trouble, setTrouble] = createSignal<string>();
  let active = true;
  onCleanup(() => {
    active = false;
  });
  void untrack(() => envRoutes.status(props.screen.transport, [{ project: props.project, env: props.env }]))
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
          path={`projects/${encodeURIComponent(props.project)}/${envSegment(props.env)}`}
        >
          {props.env}
        </RecordLink>
      </h3>
      <EnvironmentTiles status={status()} error={trouble()} />
    </section>
  );
}
