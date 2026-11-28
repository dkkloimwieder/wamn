import { createSignal, For, onCleanup, Show } from "solid-js";
import type { JSX } from "@solidjs/web";
import type { Transport } from "@wamn/web-runtime";
import { appRoutes, type AppMineResult } from "./control-client";
import { EntryPage, NavigationLink } from "./page";

type App = AppMineResult["apps"][number];
type LandingList = AppMineResult & { linksOnly: boolean };

function matches(app: App, choice: AppMineResult["default"]): boolean {
  return app.project === choice?.project && app.app === choice?.app;
}

export function Landing(props: {
  page: URL;
  navigate: (path: string) => void;
  aud: string;
  transport: Transport;
  orgAdmin: boolean;
  signOut: () => Promise<void>;
  openApp: (url: string) => void;
}): JSX.Element {
  const page = props.page;
  const controller = new AbortController();
  onCleanup(() => controller.abort());
  const [mine, setMine] = createSignal<LandingList>();
  const [saving, setSaving] = createSignal(false);
  const [trouble, setTrouble] = createSignal<string>();

  function open(list: LandingList): void {
    const selected = list.apps.length === 1 ? list.apps[0] : list.apps.find((app) => matches(app, list.default));
    if (selected && !list.linksOnly) props.openApp(selected.url);
    else setMine(list);
  }

  async function load(): Promise<void> {
    try {
      const outcome = await appRoutes.mine(props.transport, [{}], { signal: controller.signal });
      if (controller.signal.aborted) return;
      if (outcome.status !== "completed") throw new Error("The application list is unavailable.");
      const apps: App[] = [];
      const envLinks: App[] = [];
      for (const app of outcome.value.apps) {
        const host = new URL(app.url).hostname;
        if (host === page.hostname) apps.push(app);
        else if (!app.prod && host.slice(host.indexOf(".") + 1) === page.hostname) envLinks.push(app);
      }
      // A prod app on this host is local. Child hosts show the apex when there is no local app.
      const linksOnly = apps.length === 0;
      open({ ...outcome.value, apps: linksOnly ? envLinks : apps, linksOnly });
    } catch {
      if (!controller.signal.aborted) setTrouble("The application list is unavailable. Reload the page to try again.");
    }
  }

  async function makeDefault(list: LandingList, app: App): Promise<void> {
    setTrouble(undefined);
    setSaving(true);
    const choice = { project: app.project, app: app.app };
    try {
      const outcome = await appRoutes.setDefault(
        props.transport,
        [{ request_id: crypto.randomUUID(), value: choice }],
        { signal: controller.signal },
      );
      if (controller.signal.aborted) return;
      if (outcome.status !== "completed") throw new Error("The default application could not be saved.");
      open({ ...list, default: choice });
    } catch {
      if (!controller.signal.aborted) setTrouble("The default application could not be saved. Try again.");
    } finally {
      if (!controller.signal.aborted) setSaving(false);
    }
  }

  void load();
  return (
    <EntryPage title="Applications">
      <Show when={mine()}>
        {(list) => (
          <>
            <Show when={list().linksOnly}>
              <p>No application is available on this host.</p>
            </Show>
            <Show when={list().apps.length > 0}>
              <ul>
                <For each={list().apps}>
                  {(app) => (
                    <li>
                      <NavigationLink href={app.url}>
                        {app.project} / {app.app} ({app.env})
                      </NavigationLink>{" "}
                      <Show when={!list().linksOnly}>
                        <Show when={!matches(app, list().default)} fallback={<span>Default</span>}>
                          <button type="button" disabled={saving()} onClick={() => void makeDefault(list(), app)}>
                            Make default
                          </button>
                        </Show>
                      </Show>
                    </li>
                  )}
                </For>
              </ul>
            </Show>
          </>
        )}
      </Show>
      <Show when={trouble()}>{(message) => <p role="alert">{message()}</p>}</Show>
      <Show when={mine() && props.orgAdmin}>
        <p>
          <NavigationLink href={`/${props.aud}`} navigate={props.navigate}>
            Org console
          </NavigationLink>
        </p>
      </Show>
      <button type="button" onClick={() => void props.signOut()}>
        Sign out
      </button>
    </EntryPage>
  );
}
