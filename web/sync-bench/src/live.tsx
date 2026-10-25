import { Loading, Show, createEffect, createSignal, onCleanup } from "solid-js";
import { render } from "@solidjs/web";
import { Theme } from "@wfe/components";
import { outbox, shell } from "@wfe/sync";
import "@wfe/components/styles.css";
import { createSend, createTransport, keepSession, outboxDatabase, renew, rowsDatabase, rowsPersistence, type SessionState } from "@wamn/web-runtime";
import { create, PACKAGING_CREATE_ROUTE, type PackagingCreateRequest } from "@wamn/wms-client/packaging.js";
import { packagingCollection, packagingFeed } from "./packaging-feed";
import { packagingAddress } from "./packaging-writes";
import { PackagingScreen } from "./packaging-screen";
import { savedUser } from "./saved-user";
import "./style.css";

/** The audience in the route scopes the session and the user-owned outbox. */
const address = packagingAddress(location.pathname);
const pollMs = Number(new URLSearchParams(location.search).get("poll") ?? 10_000);
const lastUser = savedUser(address.application, address.environment);
void shell({ workerUrl: `${import.meta.env.BASE_URL}sw.js`, persist: false });

function SessionPackaging(props: { userId: string; signedIn: boolean; onRenew: (next: SessionState) => void }) {
  const userId = props.userId;
  const transport = createTransport({ baseUrl: `${location.origin}/wamn`, cookie: true });
  const send = createSend<Record<string, PackagingCreateRequest>>({
    transport,
    bindings: { [PACKAGING_CREATE_ROUTE.operation]: create },
    renew: async () => {
      const session = await renew(address.aud);
      props.onRenew(session ? { status: "signedIn", ...session } : { status: "signedOut" });
      return session?.userId === userId;
    },
  });
  const o = outbox({
    database: outboxDatabase(address.application, address.environment, userId),
    send: (entry, signal) => props.signedIn ? send(entry, signal) : Promise.resolve({ ok: false, retriable: true }),
    probe: async () => props.signedIn && await transport.probe(),
  });
  const c = packagingCollection(packagingFeed(transport), {}, {
    pollMs,
    persistence: rowsPersistence(rowsDatabase(address.application, address.environment, userId)),
  });
  createEffect(() => props.signedIn, (signedIn) => {
    if (signedIn) void o.replay().catch(() => undefined);
  });
  onCleanup(() => {
    o.close();
    queueMicrotask(() => void c.cleanup());
  });
  return (
    <Loading>
      <PackagingScreen collection={c} outbox={o} signedIn={props.signedIn} />
    </Loading>
  );
}

function App() {
  const [state, setState] = createSignal<SessionState>();
  const [userId, setUserId] = createSignal<string | undefined>(lastUser.load());
  const [error, setError] = createSignal("");
  const signedIn = (id: string) => {
    const current = state();
    return current?.status === "signedIn" && current.userId === id;
  };
  const options = {
    aud: address.aud,
    onState: (next: SessionState) => {
      setState(next);
      if (next.status === "signedIn") {
        lastUser.save(next.userId);
        setUserId(next.userId);
      }
    },
  };
  let keeper = keepSession(options);
  const online = () => {
    keeper.stop();
    setState(undefined);
    keeper = keepSession(options);
  };
  window.addEventListener("online", online);
  onCleanup(() => {
    window.removeEventListener("online", online);
    keeper.stop();
  });
  return (
    <Theme class="bench-page">
      <Show when={state() && state()?.status !== "signedIn"}>
        <form
          class="bench-controls"
          onSubmit={async (event) => {
            event.preventDefault();
            const form = event.currentTarget;
            const data = new FormData(form);
            setError("");
            try {
              await keeper.signIn(String(data.get("email")), String(data.get("password")));
              form.reset();
            } catch (cause) {
              setError(String(cause));
            }
          }}
        >
          <p>Sign in to upload.</p>
          <label>
            Email <input name="email" type="email" required />
          </label>
          <label>
            Password <input name="password" type="password" required />
          </label>
          <button type="submit">Sign in</button>
          <p role="alert">{error()}</p>
        </form>
      </Show>
      <main class="bench-shapes">
        <Show when={userId()} keyed>
          {(id) => <SessionPackaging userId={id} signedIn={signedIn(id)} onRenew={options.onState} />}
        </Show>
      </main>
    </Theme>
  );
}

render(() => <App />, document.getElementById("root")!);
