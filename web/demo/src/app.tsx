/**
 * The whole demo page.
 *
 * It signs in, builds one transport, and mounts the generated components under
 * it. There is no router, no navigation and no layout system, because the page
 * exists to judge the components and is deleted afterward.
 */

import { For, Show, createMemo, createSignal } from "solid-js";

import { createTransport, type Outcome } from "@wamn/web-runtime";
import { LocationListTable } from "@wamn/receiving-client/components/index.js";

import { environments, session, type Environment } from "./session.js";

export function App() {
  const [email, setEmail] = createSignal("receiving-demo@wamn.dev");
  const [password, setPassword] = createSignal("");
  const [reachable, setReachable] = createSignal<Environment[]>([]);
  const [token, setToken] = createSignal<string | null>(null);
  const [trouble, setTrouble] = createSignal<string | null>(null);
  const [outcome, setOutcome] = createSignal<string | null>(null);

  // The page is one origin, so the transport needs no base URL. The proxy
  // carries each generated path to the release.
  const transport = createMemo(() => {
    const credential = token();
    return credential === null ? null : createTransport({ baseUrl: "", credential });
  });

  async function attempt(work: () => Promise<void>) {
    setTrouble(null);
    try {
      await work();
    } catch (error) {
      setTrouble(String(error));
    }
  }

  return (
    <main>
      <h1>Receiving demo</h1>

      <Show when={token() === null}>
        <form
          onSubmit={(event) => {
            event.preventDefault();
            void attempt(async () => {
              setReachable(await environments(email(), password()));
            });
          }}
        >
          <label>
            email
            <input
              type="email"
              value={email()}
              onInput={(event) => setEmail(event.currentTarget.value)}
            />
          </label>
          <label>
            password
            <input
              type="password"
              value={password()}
              onInput={(event) => setPassword(event.currentTarget.value)}
            />
          </label>
          <button type="submit">sign in</button>
        </form>

        <ul>
          <For each={reachable()}>
            {(environment) => (
              <li>
                <button
                  type="button"
                  onClick={() =>
                    void attempt(async () => {
                      setToken(await session(email(), password(), environment.aud));
                    })
                  }
                >
                  {environment.org}/{environment.project}/{environment.env}
                </button>
              </li>
            )}
          </For>
        </ul>
      </Show>

      <Show when={trouble()}>
        <p>{trouble()}</p>
      </Show>

      <Show when={transport()}>
        {(ready) => (
          <section>
            <p>signed in, and the token is held in memory</p>
            <Show when={outcome()}>
              <p>last outcome: {outcome()}</p>
            </Show>
            <h2>location.list</h2>
            <LocationListTable
              transport={ready()}
              onOutcome={(result: Outcome<unknown>) => setOutcome(describe(result))}
            />
          </section>
        )}
      </Show>
    </main>
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
