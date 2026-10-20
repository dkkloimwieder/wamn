import type { JSX } from "@solidjs/web";
import { Show } from "solid-js";
import { Theme } from "@wfe/components";
import { DashboardPanel, DashboardShell, type DashboardShellProps } from "@wfe/components/layout";
import "./page.css";

export function Page(props: { title?: string; class?: string; children?: JSX.Element }): JSX.Element {
  return (
    <DashboardPanel class={props.class}>
      <Show when={props.title}>{(title) => <h1>{title()}</h1>}</Show>
      {props.children}
    </DashboardPanel>
  );
}

export function Frame(props: DashboardShellProps): JSX.Element {
  return (
    <Theme>
      <DashboardShell {...props} style={{ height: "100dvh", ...props.style }} />
    </Theme>
  );
}

export function NotFound(): JSX.Element {
  return (
    <Page title="No page here">
      <p>This address names no page.</p>
      <a href="/">Go to sign in</a>
    </Page>
  );
}
