import type { JSX } from "@solidjs/web";
import { createEffect, Errored, Show, untrack } from "solid-js";
import { Theme } from "@wfe/components";
import type { ScreenProps } from "./shell";
import { DashboardPanel, DashboardShell, EntryPanel, type DashboardShellProps } from "@wfe/components/layout";
import "./page.css";

export function Page(props: {
  title?: string;
  actions?: JSX.Element;
  class?: string;
  children?: JSX.Element;
}): JSX.Element {
  return (
    <DashboardPanel class={props.class}>
      <Show when={props.title}>
        {(title) => (
          <div class="ui-form-actions" style={{ "align-items": "center", "justify-content": "space-between" }}>
            <h1>{title()}</h1>
            <Show when={props.actions}>
              <div class="ui-form-actions" role="group" aria-label="Record actions">
                {props.actions}
              </div>
            </Show>
          </div>
        )}
      </Show>
      {props.children}
    </DashboardPanel>
  );
}

export function RecordLink(props: { screen: ScreenProps; path: string; children: JSX.Element }): JSX.Element {
  return (
    <NavigationLink href={`/${props.screen.params.aud}/${props.path}`} navigate={() => props.screen.open(props.path)}>
      {props.children}
    </NavigationLink>
  );
}

export function NavigationLink(props: {
  href: string;
  navigate?: (path: string) => void;
  children: JSX.Element;
}): JSX.Element {
  return (
    <a
      href={props.href}
      style={{ color: "var(--ui-accent)" }}
      onClick={(event) => {
        if (!props.navigate) return;
        if (
          event.defaultPrevented ||
          event.button !== 0 ||
          event.metaKey ||
          event.ctrlKey ||
          event.shiftKey ||
          event.altKey
        )
          return;
        event.preventDefault();
        props.navigate(props.href);
      }}
    >
      {props.children}
    </a>
  );
}

export function EntryPage(props: { title: string; children: JSX.Element }): JSX.Element {
  return (
    <Theme>
      <main>
        <EntryPanel>
          <h1>{props.title}</h1>
          {props.children}
        </EntryPanel>
      </main>
    </Theme>
  );
}

export function Frame(props: DashboardShellProps): JSX.Element {
  return (
    <Theme>
      <DashboardShell {...props} style={{ height: "100dvh", ...props.style }} />
    </Theme>
  );
}

export function NotFound(props: { home?: string; message?: string }): JSX.Element {
  return (
    <EntryPage title="No page here">
      <p>{props.message ?? "This address names no page."}</p>
      <a style={{ color: "var(--ui-accent)" }} href={props.home ?? "/"}>
        {props.home ? "Go to audience home" : "Choose an audience"}
      </a>
    </EntryPage>
  );
}

class PageNotFound extends Error {}

/** Replace the entire dashboard when a screen cannot find its requested detail. */
export function MissingPage(): never {
  throw new PageNotFound();
}

export function NotFoundBoundary(props: { home: string; path: string; children: JSX.Element }): JSX.Element {
  return (
    <Errored
      fallback={(error, reset) => {
        if (!(error() instanceof PageNotFound)) throw error();
        const failedPath = untrack(() => props.path);
        createEffect(
          () => props.path,
          (path) => {
            if (path !== failedPath) reset();
          },
        );
        return <NotFound home={props.home} />;
      }}
    >
      {props.children}
    </Errored>
  );
}
