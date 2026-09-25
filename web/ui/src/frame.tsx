/**
 * The two page frames of an application, so that the shell states no class.
 *
 * AppFrame is the signed-in page: a sidebar with the navigation, a header with
 * the context and the actions, and the screen below it. CardPage is one card in
 * the middle of an empty page, for signing in and for an address with no page.
 */

import { For, type Component, type JSX } from "solid-js";

import { Card, CardContent, CardHeader, CardTitle } from "./components/ui/card";
import {
  Sidebar,
  SidebarContent,
  SidebarGroup,
  SidebarGroupContent,
  SidebarGroupLabel,
  SidebarHeader,
  SidebarInset,
  SidebarMenu,
  SidebarMenuButton,
  SidebarMenuItem,
  SidebarProvider,
  SidebarTrigger,
} from "./components/ui/sidebar";

/** One entry of the navigation. */
export interface FrameItem {
  readonly label: string;
  readonly href: string;
  /** True while the page shows this entry. */
  readonly active: () => boolean;
}

/** One labeled group of entries. */
export interface FrameSection {
  readonly label: string;
  readonly items: readonly FrameItem[];
}

/** The link an entry renders as, which the router supplies. */
export type FrameLink = Component<{ href: string; class?: string | undefined; children?: JSX.Element }>;

export interface AppFrameProps {
  readonly title: string;
  readonly sections: readonly FrameSection[];
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
          <For each={props.sections}>
            {(section) => (
              <SidebarGroup>
                <SidebarGroupLabel>{section.label}</SidebarGroupLabel>
                <SidebarGroupContent>
                  <SidebarMenu>
                    <For each={section.items}>
                      {(item) => (
                        <SidebarMenuItem>
                          <SidebarMenuButton as={props.link} href={item.href} isActive={item.active()}>
                            {item.label}
                          </SidebarMenuButton>
                        </SidebarMenuItem>
                      )}
                    </For>
                  </SidebarMenu>
                </SidebarGroupContent>
              </SidebarGroup>
            )}
          </For>
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
