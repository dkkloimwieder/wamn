import { createEffect, createMemo, createSignal, For, Show, untrack } from "solid-js";
import {
  SidebarContextSwitcher,
  SidebarGroup,
  type SidebarContext,
  type DashboardShellProps,
} from "@wfe/components/layout";
import { FieldError } from "@wfe/components/fields";
import { control, environment, member, user } from "@wamn/control-org-client";
import type { Transport } from "@wamn/web-runtime";
import type { Address } from "./address";
import type { ScreenProps } from "./shell";
import { value } from "./outcome";

type Navigation = Parameters<NonNullable<DashboardShellProps["navigation"]>>[0];

export function ControlNavigation(props: {
  org: string;
  aud: string;
  mine: control.ControlMineResult;
  holds: ScreenProps["holds"];
  params: Readonly<Record<string, string | undefined>>;
  address: Address;
  transport: Transport;
  navigation: Navigation;
}) {
  const root = () => `/${props.aud}`;
  const path = () => props.address.url().pathname;
  const project = () => (path().startsWith(`${root()}/projects/`) ? props.params.project : undefined);
  const env = () => (project() ? props.params.env : undefined);
  const principal = () => (path().startsWith(`${root()}/users/`) ? props.params.principal : undefined);
  const projectPath = () => `${root()}/projects/${encodeURIComponent(project()!)}`;
  const environmentPath = () => `${projectPath()}/${encodeURIComponent(env()!)}`;
  const projects = createMemo(() =>
    props.mine.projects.filter((entry) => props.holds(member.MEMBER_LIST_ROUTE.operation, entry.project)),
  );
  const [environments, setEnvironments] = createSignal<readonly environment.EnvironmentListResultEnvironments[]>([]);
  const [trouble, setTrouble] = createSignal<string>();
  const [projectsOpen, setProjectsOpen] = createSignal(true);
  const [environmentsOpen, setEnvironmentsOpen] = createSignal(true);
  const [usersOpen, setUsersOpen] = createSignal(true);
  const projectNames = () => [
    ...new Set([...projects().map((entry) => entry.project), ...(project() ? [project()!] : [])]),
  ];
  const environmentNames = () => [
    ...new Set([...environments().map((entry) => entry.name), ...(env() ? [env()!] : [])]),
  ];
  const expanded = (open: boolean) => open && !props.navigation.compact();
  const projectsActive = () => path() === `${root()}/projects` || path() === `${root()}/create-project`;
  const userPath = () => `${root()}/users/${encodeURIComponent(principal()!)}`;
  const userSection = () => (path().startsWith(`${userPath()}/projects`) ? "projects" : "memberships");
  createEffect(project, (name) => {
    setEnvironments([]);
    setTrouble(undefined);
    if (!name || !untrack(() => props.holds(environment.ENVIRONMENT_LIST_ROUTE.operation, name))) return;
    let active = true;
    environment
      .list(
        untrack(() => props.transport),
        [{ project: name }],
      )
      .then((result) => {
        if (active) setEnvironments(value(result).environments);
      })
      .catch((error: unknown) => {
        if (active) setTrouble(String(error));
      });
    return () => {
      active = false;
    };
  });
  const organization = (): SidebarContext => ({ id: `${root()}/projects`, kind: "Organization", name: props.org });
  const currentProject = (): SidebarContext => ({ id: projectPath(), kind: "Project", name: project()! });
  const current = (): SidebarContext =>
    env()
      ? { id: environmentPath(), kind: "Environment", name: `${project()}/${env()}` }
      : project()
        ? currentProject()
        : organization();
  const choices = (): SidebarContext[] =>
    env()
      ? environments().map((entry) => ({
          id: `${projectPath()}/${encodeURIComponent(entry.name)}`,
          kind: "Environment",
          name: `${project()}/${entry.name}`,
        }))
      : project()
        ? projects()
            .filter((entry) => entry.project !== project())
            .map((entry) => ({
              id: `${root()}/projects/${encodeURIComponent(entry.project)}`,
              kind: "Project",
              name: entry.project,
            }))
        : [];
  const navigate = (href: string) => {
    props.address.navigate(href);
    props.navigation.close();
  };
  function Link(link: { href: string; label: string; close?: () => void; active?: boolean }) {
    return (
      <a
        href={link.href}
        data-ui-text="system"
        title={link.label}
        aria-current={(link.active ?? path() === link.href) ? "page" : undefined}
        onClick={(event) => {
          if (event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
          event.preventDefault();
          navigate(link.href);
          link.close?.();
        }}
      >
        {link.label}
      </a>
    );
  }
  return (
    <div class="ui-dashboard-menu">
      <SidebarContextSwitcher
        value={current()}
        options={choices()}
        parent={env() ? currentProject() : project() ? organization() : undefined}
        compact={props.navigation.compact()}
        onValueChange={(context) => navigate(context.id)}
      />
      <Show when={trouble()}>{(message) => <FieldError role="alert">{message()}</FieldError>}</Show>
      <Show when={!env()}>
        <SidebarGroup
          label="Projects"
          compact={props.navigation.compact()}
          open={projectsOpen()}
          onOpenChange={setProjectsOpen}
          onExpand={() => setProjectsOpen(true)}
          active={projectsActive() || (path() === projectPath() && !expanded(projectsOpen()))}
        >
          {(close) => (
            <>
              <Link href={`${root()}/projects`} label="All projects" close={close} active={false} />
              <For each={projectNames()}>
                {(name) => (
                  <Link
                    href={`${root()}/projects/${encodeURIComponent(name)}`}
                    label={name}
                    close={close}
                    active={expanded(projectsOpen()) && path() === `${root()}/projects/${encodeURIComponent(name)}`}
                  />
                )}
              </For>
            </>
          )}
        </SidebarGroup>
      </Show>
      <Show when={!project()}>
        <Show when={props.holds(user.USER_LIST_ROUTE.operation)}>
          <Show
            when={principal()}
            fallback={
              <Link
                href={`${root()}/users`}
                label="Users"
                active={path() === `${root()}/users` || path() === `${root()}/invite-user`}
              />
            }
          >
            <SidebarGroup
              label="Users"
              compact={props.navigation.compact()}
              open={usersOpen()}
              onOpenChange={setUsersOpen}
              onExpand={() => setUsersOpen(true)}
              active={path() === userPath() || !expanded(usersOpen())}
            >
              {(close) => (
                <>
                  <Link href={`${root()}/users`} label="All users" close={close} />
                  <Link
                    href={`${root()}/users/${encodeURIComponent(principal()!)}/projects`}
                    label="Project access"
                    close={close}
                    active={expanded(usersOpen()) && path() !== userPath() && userSection() === "projects"}
                  />
                  <Link
                    href={`${root()}/users/${encodeURIComponent(principal()!)}/memberships`}
                    label="Memberships"
                    close={close}
                    active={expanded(usersOpen()) && path() !== userPath() && userSection() === "memberships"}
                  />
                </>
              )}
            </SidebarGroup>
          </Show>
        </Show>
      </Show>
      <Show when={project()}>
        <SidebarGroup
          label="Environments"
          compact={props.navigation.compact()}
          open={environmentsOpen()}
          onOpenChange={setEnvironmentsOpen}
          onExpand={() => setEnvironmentsOpen(true)}
          active={
            path() === `${projectPath()}/environments` ||
            (path() === environmentPath() && !expanded(environmentsOpen()))
          }
        >
          {(close) => (
            <>
              <Link href={`${projectPath()}/environments`} label="All environments" close={close} active={false} />
              <For each={environmentNames()}>
                {(name) => (
                  <Link
                    href={`${projectPath()}/${encodeURIComponent(name)}`}
                    label={name}
                    close={close}
                    active={expanded(environmentsOpen()) && path() === `${projectPath()}/${encodeURIComponent(name)}`}
                  />
                )}
              </For>
            </>
          )}
        </SidebarGroup>
      </Show>
      <Show when={project() && !env()}>
        <Link
          href={`${projectPath()}/members`}
          label="Members"
          active={path() === `${projectPath()}/members` || path().startsWith(`${projectPath()}/members/`)}
        />
      </Show>
      <Show when={env()}>
        <For
          each={
            [
              ["workloads", "Workloads"],
              ["live-releases", "Live releases"],
              ["operations", "Operations"],
              ["roles", "Roles"],
              ["members", "Members"],
            ] as const
          }
        >
          {([route, label]) => (
            <Link
              href={`${environmentPath()}/${route}`}
              label={label}
              active={path() === `${environmentPath()}/${route}` || path().startsWith(`${environmentPath()}/${route}/`)}
            />
          )}
        </For>
      </Show>
    </div>
  );
}
