/**
 * The whole demo page.
 *
 * It signs in by cookie, builds one transport, and mounts the generated
 * components of one application under it. WAMN_DEMO_APP selects the
 * application when Vite starts. There is no router, no navigation and no layout system,
 * because the page exists to judge the components and is deleted afterward.
 */

import { For, Show, createMemo, createSignal } from "solid-js";

import {
  Button,
  Card,
  CardContent,
  CardHeader,
  CardTitle,
  Field,
  FieldGroup,
  FieldLabel,
  Input,
  useColorMode,
} from "@wamn/ui";
import {
  createTransport,
  environments,
  keepSession,
  type Environment,
  type Outcome,
  type SessionKeeper,
} from "@wamn/web-runtime";
import { ReceivingScreens } from "./receiving.js";
import { WmsScreens } from "./wms.js";

/** What each application brings to the page. */
const PAGES = {
  receiving: { title: "Receiving demo", account: "receiving-demo@wamn.dev", Screens: ReceivingScreens },
  wms: { title: "WMS demo", account: "wms-demo@wamn.dev", Screens: WmsScreens },
} as const;

/** The application that Vite selected when it started. */
const page = PAGES[__WAMN_DEMO_APP__];
document.title = page.title;

/**
 * The environment this page signed in to, from the address.
 *
 * The page keeps nothing in browser storage, so a reload reads the audience
 * from the fragment that the sign in wrote, and the keeper renews from the
 * renewal cookie.
 */
function addressedAudience(): string | null {
  const aud = new URLSearchParams(window.location.hash.slice(1)).get("aud");
  return aud === null || aud === "" ? null : aud;
}

export function App() {
  const [email, setEmail] = createSignal<string>(page.account);
  const [password, setPassword] = createSignal("");
  const [reachable, setReachable] = createSignal<Environment[]>([]);
  const [signedIn, setSignedIn] = createSignal(false);
  const [trouble, setTrouble] = createSignal<string | null>(null);
  const [outcome, setOutcome] = createSignal<string | null>(null);

  let keeper: SessionKeeper | null = null;
  function keep(aud: string): SessionKeeper {
    keeper?.stop();
    keeper = keepSession({
      aud,
      onState: (state) => {
        setSignedIn(state.status === "signedIn");
        if (state.status === "failed") {
          setTrouble(state.reason);
        }
      },
    });
    return keeper;
  }
  const addressed = addressedAudience();
  if (addressed !== null) {
    keep(addressed);
  }

  // The page is one origin, so the transport needs no base URL. The proxy
  // carries each generated path to the release, and the browser carries the
  // session cookie.
  const transport = createMemo(() =>
    signedIn() ? createTransport({ baseUrl: "", cookie: true }) : null,
  );

  async function attempt(work: () => Promise<void>) {
    setTrouble(null);
    try {
      await work();
    } catch (error) {
      setTrouble(String(error));
    }
  }

  async function signOut() {
    await keeper?.signOut();
  }

  const read = (result: Outcome<unknown>) => setOutcome(describe(result));
  const { colorMode, toggleColorMode } = useColorMode();

  return (
    <div class="min-h-screen">
      <header class="border-b">
        <div class="mx-auto flex max-w-screen-2xl items-center justify-between gap-4 px-6 py-3">
          <div class="flex items-baseline gap-3">
            <h1 class="text-base font-semibold uppercase">{page.title}</h1>
            <Show when={signedIn()}>
              <span class="text-xs text-muted-foreground">
                signed in, and the browser holds the session in an HttpOnly cookie
              </span>
            </Show>
          </div>
          <div class="flex items-center gap-3">
            <Show when={outcome()}>
              <span class="text-xs text-muted-foreground">last outcome: {outcome()}</span>
            </Show>
            <Button variant="outline" size="sm" onClick={toggleColorMode}>
              {colorMode() === "dark" ? "light mode" : "dark mode"}
            </Button>
            <Show when={signedIn()}>
              <Button variant="outline" size="sm" onClick={() => void attempt(signOut)}>
                sign out
              </Button>
            </Show>
          </div>
        </div>
      </header>

      <main class="mx-auto max-w-screen-2xl px-6 py-6">
        <Show when={!signedIn()}>
          <div class="flex min-h-[70vh] items-center justify-center p-4">
            <Card class="w-full max-w-sm">
              <CardHeader>
                <CardTitle>
                  <h2>Sign in</h2>
                </CardTitle>
              </CardHeader>
              <CardContent class="flex flex-col gap-6">
                <form
                  onSubmit={(event) => {
                    event.preventDefault();
                    void attempt(async () => {
                      setReachable(await environments(email(), password()));
                    });
                  }}
                >
                  <FieldGroup>
                    <Field>
                      <FieldLabel for="demo-email">email</FieldLabel>
                      <Input
                        id="demo-email"
                        type="email"
                        value={email()}
                        onInput={(event) => setEmail(event.currentTarget.value)}
                      />
                    </Field>
                    <Field>
                      <FieldLabel for="demo-password">password</FieldLabel>
                      <Input
                        id="demo-password"
                        type="password"
                        value={password()}
                        onInput={(event) => setPassword(event.currentTarget.value)}
                      />
                    </Field>
                    <Button type="submit">sign in</Button>
                  </FieldGroup>
                </form>

                <ul class="flex flex-col gap-2">
                  <For each={reachable()}>
                    {(environment) => (
                      <li>
                        <Button
                          variant="outline"
                          class="w-full"
                          onClick={() =>
                            void attempt(async () => {
                              const fragment = new URLSearchParams({ aud: environment.aud });
                              window.location.hash = fragment.toString();
                              await keep(environment.aud).signIn(email(), password());
                            })
                          }
                        >
                          {environment.org}/{environment.project}/{environment.env}
                        </Button>
                      </li>
                    )}
                  </For>
                </ul>
                <Show when={trouble()}>
                  <p class="text-sm text-destructive">{trouble()}</p>
                </Show>
              </CardContent>
            </Card>
          </div>
        </Show>

        <Show when={signedIn() && trouble()}>
          <p class="mb-6 text-sm text-destructive">{trouble()}</p>
        </Show>

        <Show when={transport()}>
          {(ready) => (
            <div class="flex flex-col gap-6">
              <page.Screens transport={ready()} read={read} />
            </div>
          )}
        </Show>
      </main>
    </div>
  );
}

/** One line for whatever the release answered. */
function describe(result: Outcome<unknown>): string {
  switch (result.status) {
    case "completed":
      return "completed";
    case "partiallyCompleted":
      return "partially completed";
    case "refused":
      return `refused ${result.code ?? ""} ${JSON.stringify(result.detail ?? null)}`;
    case "uncertain":
      return `uncertain ${result.reason}`;
  }
}
