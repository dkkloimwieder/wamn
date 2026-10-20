import type { JSX } from "@solidjs/web";
import { Show } from "solid-js";
import { uiScope } from "@wfe/components";
import { DashboardPanel, DashboardShell, type DashboardShellProps } from "@wfe/components/layout";
import { appearance } from "./appearance";
import "./appearance.css";

export function Page(props: { title?: string; children?: JSX.Element }): JSX.Element {
  return (
    <DashboardPanel>
      <Show when={props.title}>{(title) => <h1>{title()}</h1>}</Show>
      {props.children}
    </DashboardPanel>
  );
}

export function Frame(props: DashboardShellProps): JSX.Element {
  const scope = uiScope();
  return (
    <DashboardShell
      {...scope}
      {...props}
      class="platform-appearance"
      style={{ ...scope.style, ...appearance, height: "100dvh", ...props.style }}
    />
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
