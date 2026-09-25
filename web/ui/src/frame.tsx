/**
 * The two page frames of an application, so that the shell states no class.
 *
 * AppFrame is the signed-in page: a sidebar with the navigation, a header with
 * the context and the actions, and the screen below it. The navigation is a list
 * of entries, and an entry is one link or a labeled group of links. CardPage is one card in
 * the middle of an empty page, for signing in and for an address with no page.
 * ScreenActions is the row of buttons above a screen.
 */

import { For, type Component, type JSX } from "solid-js";

import { Card, CardContent, CardHeader, CardTitle } from "./components/ui/card";
import {
  Sidebar,
  SidebarContent,
  SidebarGroup,
  SidebarGroupContent,
  SidebarHeader,
  SidebarInset,
  SidebarMenu,
  SidebarMenuButton,
  SidebarMenuItem,
  SidebarMenuSub,
  SidebarMenuSubButton,
  SidebarMenuSubItem,
  SidebarProvider,
  SidebarTrigger,
} from "./components/ui/sidebar";

/** One link of the navigation. */
export interface FrameItem {
  readonly label: string;
  readonly href: string;
  /** True while the page shows this entry. */
  readonly active: () => boolean;
}

/** One labeled group of links. */
export interface FrameSection {
  readonly label: string;
  readonly items: readonly FrameItem[];
}

/** One entry of the navigation: a link, or a group of links under a label. */
export type FrameEntry = FrameItem | FrameSection;

/** The link an entry renders as, which the router supplies. */
export type FrameLink = Component<{ href: string; class?: string | undefined; children?: JSX.Element }>;

export interface AppFrameProps {
  readonly title: string;
  readonly navigation: readonly FrameEntry[];
  readonly link: FrameLink;
  /** A short line in the header, for example the environment. */
  readonly context: JSX.Element;
  /** The buttons at the end of the header. */
  readonly actions: JSX.Element;
  readonly children: JSX.Element;
}

export function AppFrame(props: AppFrameProps): JSX.Element {
  return (
    <SidebarProvider>
      <Sidebar>
        <SidebarHeader>
          <span class="px-2 py-1 text-sm font-semibold uppercase">{props.title}</span>
        </SidebarHeader>
        <SidebarContent>
          <SidebarGroup>
            <SidebarGroupContent>
              <SidebarMenu>
                <For each={props.navigation}>
                  {(entry) => (
                    <SidebarMenuItem>
                      {"items" in entry ? (
                        <>
                          <span class="flex h-8 items-center px-2 text-sm">{entry.label}</span>
                          <SidebarMenuSub>
                            <For each={entry.items}>
                              {(item) => (
                                <SidebarMenuSubItem>
                                  <SidebarMenuSubButton as={props.link} href={item.href} isActive={item.active()}>
                                    {item.label}
                                  </SidebarMenuSubButton>
                                </SidebarMenuSubItem>
                              )}
                            </For>
                          </SidebarMenuSub>
                        </>
                      ) : (
                        <SidebarMenuButton as={props.link} href={entry.href} isActive={entry.active()}>
                          {entry.label}
                        </SidebarMenuButton>
                      )}
                    </SidebarMenuItem>
                  )}
                </For>
              </SidebarMenu>
            </SidebarGroupContent>
          </SidebarGroup>
        </SidebarContent>
      </Sidebar>
      <SidebarInset class="min-w-0">
        <header class="flex items-center gap-3 border-b px-4 py-2">
          <SidebarTrigger />
          <span class="min-w-0 flex-1 truncate text-xs text-muted-foreground">{props.context}</span>
          {props.actions}
        </header>
        <div class="min-w-0 p-6">{props.children}</div>
      </SidebarInset>
    </SidebarProvider>
  );
}

export interface ScreenActionsProps {
  readonly children: JSX.Element;
}

export function ScreenActions(props: ScreenActionsProps): JSX.Element {
  return <div class="mb-4 flex flex-wrap gap-2">{props.children}</div>;
}

export interface CardPageProps {
  readonly title: string;
  readonly children: JSX.Element;
}

export function CardPage(props: CardPageProps): JSX.Element {
  return (
    <div class="flex min-h-screen items-center justify-center p-4">
      <Card class="w-full max-w-sm">
        <CardHeader>
          <CardTitle>
            <h1>{props.title}</h1>
          </CardTitle>
        </CardHeader>
        <CardContent class="flex flex-col gap-6">{props.children}</CardContent>
      </Card>
    </div>
  );
}
