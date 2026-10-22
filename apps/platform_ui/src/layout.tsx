import { createMemo, For, Show } from "solid-js";
import type { JSX } from "@solidjs/web";
import { Breadcrumbs, DropdownMenu, Theme } from "@wfe/components";
import { SidebarContextSwitcher } from "@wfe/components/layout";
import { ControlNavigation } from "./control-navigation";
import type { Transport } from "@wamn/web-runtime";
import { defineShell, ShellView } from "@wfe/shell";
import { MissingPage, NavigationLink, Page } from "./page";
import { matchPath, queryValues, type Address } from "./address";
import type { ScreenProps, ShellSection } from "./shell";

/** Load the layout only after the session and its permissions are ready. */
export default function Layout(props: {
  title: string;
  aud: string;
  sections: readonly ShellSection[];
  holds: ScreenProps["holds"];
  address: Address;
  controlMine?: ScreenProps["controlMine"];
  transport: Transport;
  signOut: () => Promise<void>;
  routes: readonly ShellSection[];
}): JSX.Element {
  const application = () => {
    const parts = props.aud.split(":");
    return parts[2] === "project-env" ? { org: parts[3]!, project: parts[4]!, env: parts[5]! } : undefined;
  };
  const projectPath = () => {
    const scope = application()!;
    return `/urn:wamn:control:${scope.org}/projects/${encodeURIComponent(scope.project)}`;
  };
  const follow = (path: string, close?: () => void) => (event: MouseEvent) => {
    if (event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
    event.preventDefault();
    props.address.navigate(path);
    close?.();
  };
  const path = () => props.address.url().pathname.split("/").slice(2).join("/");
  const selected = createMemo(() => {
    for (const section of props.routes)
      for (const route of [...section.screens, ...(section.routes ?? [])]) {
        const params = matchPath(route.path, path());
        if (!params || !props.holds(route.operation, params.project)) continue;
        params.aud = props.aud;
        return { route, home: section.screens[0], params };
      }
    return undefined;
  });
  const description = createMemo(() =>
    defineShell({
      id: "platform",
      audiences: [{ id: props.aud, label: props.title }],
      routes: [
        {
          id: props.address.url().pathname,
          label: selected()?.route.label ?? (selected() ? props.title : "No screens available"),
          view: "page",
          audience: props.aud,
        },
      ],
    }),
  );
  function Destination(): JSX.Element {
    const current = selected();
    if (!current)
      return (
        <Page title="No screens available">
          <p>This audience has no available screens.</p>
          <NavigationLink href="/" navigate={props.address.navigate}>
            Choose an audience
          </NavigationLink>
        </Page>
      );
    const { route, home, params } = current;
    const open = (path: string) => props.address.navigate(`/${props.aud}/${path}`, false, true);
    const close = () =>
      window.history.state?.opened === true
        ? window.history.back()
        : props.address.navigate(
            `/${props.aud}/${home && props.holds(home.operation) ? home.path : (props.sections[0]?.screens[0]?.path ?? "")}`,
            true,
          );
    return (
      <route.component
        actions={(route.actions ?? [])
          .filter((action) => props.holds(action.operation))
          .map((action) => ({
            ...action,
            path: action.path.replace(/:(\w+)/g, (_, name: string) => encodeURIComponent(params[name] ?? "")),
          }))}
        transport={props.transport}
        holds={(operation, project) => props.holds(operation, project ?? params.project)}
        controlMine={props.controlMine}
        params={params}
        search={queryValues(props.address.url().searchParams)}
        open={open}
        close={close}
      />
    );
  }
  const roleName = () => (application() ? selected()?.params.role : undefined);
  const rolePath = () => `/${props.aud}/administration/roles/${encodeURIComponent(roleName()!)}`;
  const scroll = () => selected()?.route.scroll ?? "main";
  const breadcrumbs = () => {
    const root = `/${props.aud}`;
    const scope = application();
    const entries = scope
      ? [
          { label: scope.org, href: `/urn:wamn:control:${scope.org}/projects` },
          { label: scope.project, href: projectPath() },
          { label: scope.env, href: root },
        ]
      : [{ label: props.title, href: root }];
    const parts = props.address.url().pathname.split("/").slice(2).filter(Boolean);
    const routes = props.sections.flatMap((section) => [...section.screens, ...(section.routes ?? [])]);
    const selected = routes.find((route) => matchPath(route.path, parts.join("/")));
    for (let index = 0; index < parts.length; index += 1) {
      const path = parts.slice(0, index + 1).join("/");
      const route = routes.find((route) => {
        const params = matchPath(route.path, path);
        return (
          params &&
          props.holds(route.operation, params.project) &&
          (!selected || selected.path === route.path || selected.path.startsWith(`${route.path}/`))
        );
      });
      if (route) {
        const section = props.sections.find((section) => section.routes?.includes(route));
        const home = section?.screens[0];
        if (entries.length === 1 && home && home.path !== path && props.holds(home.operation)) {
          entries.push({ label: section!.label, href: `${root}/${home.path}` });
        }
        const screenSection = props.sections.find((section) => section.screens.some((screen) => screen === route));
        const label = screenSection?.screens.length === 1 ? screenSection.label : route.label;
        const segment = route.path.split("/").at(-1)!;
        const params = matchPath(route.path, path)!;
        entries.push({
          label: label ?? params[segment.slice(1)] ?? parts[index]!,
          href: `${root}/${path}${scope && segment === ":role" ? "/permissions" : ""}`,
        });
      } else if (index === parts.length - 1) {
        entries.push({ label: "No page here", href: `${root}/${path}` });
      }
    }
    return entries;
  };
  return (
    <Show when={selected() || path() === ""} fallback={<MissingPage />}>
      <Theme>
        <ShellView
          description={description()}
          bindings={{
            audience: () => props.aud,
            route: () => props.address.url().pathname,
            href: (route) => route,
            views: { page: Destination },
            onAudienceChange: (aud) => props.address.navigate(`/${aud}`),
            onNavigate: props.address.navigate,
          }}
          style={{ height: "100dvh" }}
          scroll={scroll()}
          header={
            <>
              <Breadcrumbs aria-label="Platform location">
                <Breadcrumbs.List>
                  <For each={breadcrumbs()}>
                    {(entry, index) => (
                      <Breadcrumbs.Item>
                        <Show when={index() > 0}>
                          <Breadcrumbs.Separator />
                        </Show>
                        <Show
                          when={index() < breadcrumbs().length - 1}
                          fallback={
                            <Breadcrumbs.Current data-ui-text="system">{entry.label.toUpperCase()}</Breadcrumbs.Current>
                          }
                        >
                          <Breadcrumbs.Link href={entry.href} onClick={follow(entry.href)} data-ui-text="system">
                            {entry.label.toUpperCase()}
                          </Breadcrumbs.Link>
                        </Show>
                      </Breadcrumbs.Item>
                    )}
                  </For>
                </Breadcrumbs.List>
              </Breadcrumbs>
              <div class="ui-dashboard-header-actions">
                <DropdownMenu>
                  <DropdownMenu.Trigger aria-label="User menu">
                    User
                    <DropdownMenu.Icon />
                  </DropdownMenu.Trigger>
                  <DropdownMenu.Portal>
                    <DropdownMenu.Content>
                      <DropdownMenu.Item onSelect={() => void props.signOut()} data-ui-text="system">
                        Sign out
                      </DropdownMenu.Item>
                    </DropdownMenu.Content>
                  </DropdownMenu.Portal>
                </DropdownMenu>
              </div>
            </>
          }
          navigation={
            props.sections.length
              ? (navigation) => (
                  <Show
                    when={
                      props.controlMine &&
                      props.sections.some((section) =>
                        section.screens.some((screen) => screen.path === "projects" || screen.path === "users"),
                      )
                    }
                    fallback={
                      <div class="ui-dashboard-menu">
                        <Show when={application()}>
                          {(scope) => (
                            <SidebarContextSwitcher
                              value={
                                roleName()
                                  ? { id: rolePath(), kind: "Role", name: roleName()! }
                                  : {
                                      id: `/${props.aud}`,
                                      kind: "Environment",
                                      name: `${scope().project}/${scope().env}`,
                                    }
                              }
                              options={[]}
                              parent={
                                roleName()
                                  ? {
                                      id: `/${props.aud}/administration/roles`,
                                      kind: "Environment",
                                      name: `${scope().project}/${scope().env}`,
                                    }
                                  : { id: projectPath(), kind: "Project", name: scope().project }
                              }
                              compact={navigation.compact()}
                              onValueChange={(context) => {
                                props.address.navigate(context.id);
                                navigation.close();
                              }}
                            />
                          )}
                        </Show>
                        <Show
                          when={!roleName()}
                          fallback={
                            <For
                              each={[
                                { path: "permissions", label: "Permissions" },
                                { path: "users", label: "Users" },
                              ]}
                            >
                              {(item) => (
                                <a
                                  href={`${rolePath()}/${item.path}`}
                                  data-ui-text="system"
                                  aria-current={
                                    props.address.url().pathname === `${rolePath()}/${item.path}` ||
                                    props.address.url().pathname.startsWith(`${rolePath()}/${item.path}/`) ||
                                    (item.path === "permissions" && props.address.url().pathname === rolePath())
                                      ? "page"
                                      : undefined
                                  }
                                  onClick={follow(`${rolePath()}/${item.path}`, navigation.close)}
                                >
                                  {item.label}
                                </a>
                              )}
                            </For>
                          }
                        >
                          <For each={props.sections}>
                            {(section) => (
                              <For each={section.screens}>
                                {(item) => (
                                  <a
                                    href={`/${props.aud}/${item.path}`}
                                    data-ui-text="system"
                                    aria-current={
                                      props.address.url().pathname === `/${props.aud}/${item.path}` ||
                                      props.address.url().pathname.startsWith(`/${props.aud}/${item.path}/`) ||
                                      (item === section.screens[0] &&
                                        section.routes?.some((route) => {
                                          const params = matchPath(
                                            route.path,
                                            props.address.url().pathname.split("/").slice(2).join("/"),
                                          );
                                          return params && props.holds(route.operation, params.project);
                                        }))
                                        ? "page"
                                        : undefined
                                    }
                                    onClick={follow(`/${props.aud}/${item.path}`, navigation.close)}
                                  >
                                    {section.screens.length === 1 ? section.label : `${section.label}: ${item.label}`}
                                  </a>
                                )}
                              </For>
                            )}
                          </For>
                        </Show>
                      </div>
                    }
                  >
                    <ControlNavigation
                      org={props.title}
                      aud={props.aud}
                      mine={props.controlMine!}
                      holds={props.holds}
                      params={selected()?.params ?? {}}
                      address={props.address}
                      transport={props.transport}
                      navigation={navigation}
                    />
                  </Show>
                )
              : false
          }
        />
      </Theme>
    </Show>
  );
}
