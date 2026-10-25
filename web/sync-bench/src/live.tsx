import { Loading, Show, createSignal, onCleanup } from "solid-js";
import { render } from "@solidjs/web";
import { Theme } from "@wfe/components";
import { outbox } from "@wfe/sync";
import "@wfe/components/styles.css";
import { createSend, createTransport, keepSession, outboxDatabase, renew, type SessionState } from "@wamn/web-runtime";
import { create, PACKAGING_CREATE_ROUTE, type PackagingCreateRequest } from "@wamn/wms-client/packaging.js";
import { packagingCollection, packagingFeed } from "./packaging-feed";
import { packagingAddress } from "./packaging-writes";
import { PackagingScreen } from "./packaging-screen";
import "./style.css";

/** The audience in the route scopes the session and the user-owned outbox. */
const address = packagingAddress(location.pathname);
const pollMs = Number(new URLSearchParams(location.search).get("poll") ?? 10_000);

function SessionPackaging(props: { userId: string }) {
  const userId = props.userId;
  const transport = createTransport({ baseUrl: `${location.origin}/wamn`, cookie: true });
  const o = outbox({
    database: outboxDatabase(address.application, address.environment, userId),
    send: createSend<Record<string, PackagingCreateRequest>>({
      transport,
      bindings: { [PACKAGING_CREATE_ROUTE.operation]: create },
      renew: async () => (await renew(address.aud))?.userId === userId,
    }),
    probe: () => transport.probe(),
  });
  const c = packagingCollection(packagingFeed(transport), {}, { pollMs });
  onCleanup(() => {
    o.close();
    queueMicrotask(() => void c.cleanup());
  });
  return (
    <Loading>
      <PackagingScreen collection={c} outbox={o} />
    </Loading>
  );
}

function App() {
  const [state, setState] = createSignal<SessionState>();
  const [userId, setUserId] = createSignal<string>();
  const [error, setError] = createSignal("");
  const keeper = keepSession({
    aud: address.aud,
    onState: (next) => {
      setState(next);
      if (next.status === "signedIn") setUserId(next.userId);
    },
  });
  onCleanup(() => keeper.stop());
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
          {(id) => <SessionPackaging userId={id} />}
        </Show>
      </main>
    </Theme>
  );
}

render(() => <App />, document.getElementById("root")!);
