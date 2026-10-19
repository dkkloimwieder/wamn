import type { JSX } from "@solidjs/web";
import { Card } from "@wfe/components/display";
import type { Address } from "./address";

export function Page(props: { title: string; children?: JSX.Element }): JSX.Element {
  return (
    <Card>
      <h1>{props.title}</h1>
      {props.children}
    </Card>
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
