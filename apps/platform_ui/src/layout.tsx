import { For, Show } from "solid-js";
import type { JSX } from "@solidjs/web";
import { Breadcrumbs, DropdownMenu } from "@wfe/components";
import { Frame } from "./page";
import { matchPath, type Address } from "./address";
import type { ScreenProps, ShellSection } from "./shell";

/** Load the layout only after the session and its permissions are ready. */
export default function Layout(props: {
  title: string;
  aud: string;
  sections: readonly ShellSection[];
  holds: ScreenProps["holds"];
  address: Address;
  signOut: () => Promise<void>;
  children: JSX.Element;
}): JSX.Element {
  const follow = (path: string, close?: () => void) => (event: MouseEvent) => {
    if (event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
    event.preventDefault();
    props.address.navigate(path);
    close?.();
  };
  const scroll = () => {
    const path = props.address.url().pathname.split("/").slice(2).join("/");
    const route = props.sections
      .flatMap((section) => [...section.screens, ...(section.routes ?? [])])
      .find((route) => {
        const params = matchPath(route.path, path);
        return params && props.holds(route.operation, params.project);
      });
    return route?.scroll ?? "main";
  };
  const breadcrumbs = () => {
    const root = `/${props.aud}`;
    const entries = [{ label: props.title, href: root }];
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
          href: `${root}/${path}`,
        });
      } else if (index === parts.length - 1) {
        entries.push({ label: "No page here", href: `${root}/${path}` });
      }
    }
    return entries;
  };
  return (
    <Frame
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
          ? ({ close }) => (
              <div class="ui-dashboard-menu">
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
                          onClick={follow(`/${props.aud}/${item.path}`, close)}
                        >
                          {section.screens.length === 1 ? section.label : `${section.label}: ${item.label}`}
                        </a>
                      )}
                    </For>
                  )}
                </For>
              </div>
            )
          : undefined
      }
    >
      {props.children}
    </Frame>
  );
}
