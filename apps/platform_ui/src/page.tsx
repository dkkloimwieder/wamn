import type { JSX } from "@solidjs/web";
import { uiScope } from "@wfe/components";
import { DashboardPanel, DashboardShell, type DashboardShellProps } from "@wfe/components/layout";
import { appearance } from "./appearance";
import "./appearance.css";
import type { Address } from "./address";

export function Page(props: { title: string; children?: JSX.Element }): JSX.Element {
  return (
    <DashboardPanel>
      <h1>{props.title}</h1>
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

export function Link(props: { address: Address; href: string; children: JSX.Element }): JSX.Element {
  return (
    <a
      href={props.href}
      onClick={(event) => {
        if (event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
        event.preventDefault();
        props.address.navigate(props.href);
      }}
    >
      {props.children}
    </a>
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
