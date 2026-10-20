import { For, Show } from "solid-js";
import type { JSX } from "@solidjs/web";
import { Button } from "@wfe/components/fields";
import { DashboardPanel } from "@wfe/components/layout";
import { Frame } from "./page";
import type { Address } from "./address";
import type { ShellSection } from "./shell";

/** Load the layout only after the session and its permissions are ready. */
export default function Layout(props: {
  title: string;
  aud: string;
  sections: readonly ShellSection[];
  address: Address;
  signOut: () => Promise<void>;
  children: JSX.Element;
}): JSX.Element {
  return (
    <Frame
      header={
        <>
          <span>{props.title}</span>
          <Button onClick={() => void props.signOut()}>sign out</Button>
        </>
      }
      navigation={({ close }) => (
        <For each={props.sections}>
          {(section) => (
            <DashboardPanel class="ui-dashboard-menu">
              <Show when={section.screens.length > 1}>
                <p>{section.label}</p>
              </Show>
              <For each={section.screens}>
                {(item) => (
                  <a
                    href={`/${props.aud}/${item.path}`}
                    aria-current={
                      props.address.url().pathname === `/${props.aud}/${item.path}` ||
                      props.address.url().pathname.startsWith(`/${props.aud}/${item.path}/`)
                        ? "page"
                        : undefined
                    }
                    onClick={(event) => {
                      if (event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey)
                        return;
                      event.preventDefault();
                      props.address.navigate(`/${props.aud}/${item.path}`);
                      close();
                    }}
                  >
                    {section.screens.length === 1 ? section.label : item.label}
                  </a>
                )}
              </For>
            </DashboardPanel>
          )}
        </For>
      )}
    >
      {props.children}
    </Frame>
  );
}
