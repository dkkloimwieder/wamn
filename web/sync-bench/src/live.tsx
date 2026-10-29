import { Loading, Show, createEffect, createSignal, onCleanup } from "solid-js";
import { render } from "@solidjs/web";
import { Theme } from "@wfe/components";
import { outbox, outboxStatus, shell, type ShellResult } from "@wfe/sync";
import "@wfe/components/styles.css";
import {
  createSend,
  createTransport,
  keepSession,
  outboxDatabase,
  renew,
  rowsDatabase,
  rowsPersistence,
  savedUser,
  type SessionState,
} from "@wamn/web-runtime";
import {
  create,
  PACKAGING_CREATE_REQUEST_FIELDS,
  PACKAGING_CREATE_ROUTE,
  PACKAGING_SHAPE_DIGEST,
  type PackagingCreateRequest,
} from "./generated/wamn_wms/packaging.js";
import { packagingFeed } from "./generated/wamn_wms/packaging.js";
import { LOCATION_SHAPE_DIGEST } from "./generated/wamn_wms/location.js";
import { locationFeed } from "./generated/wamn_wms/location.js";
import { packagingOffline, packagingScope } from "./packaging-offline.js";
import type { StatusFilter } from "./order.js";
import { packagingAddress } from "./packaging-writes.js";
import { PackagingScreen } from "./packaging-screen.js";
import { canReload } from "./update.js";
import "./style.css";

/** The audience in the route scopes the session and the user-owned outbox. */
const address = packagingAddress(location.pathname);
const pollMs = Number(new URLSearchParams(location.search).get("poll") ?? 10_000);
const lastUser = savedUser(address.application, address.environment);
const [waitingReload, setWaitingReload] = createSignal<(() => void) | undefined>();
const [shellResult, setShellResult] = createSignal<ShellResult>();
void shell({
  workerUrl: `${import.meta.env.BASE_URL}sw.js`,
  onUpdate: (reload) => setWaitingReload(() => reload),
}).then(setShellResult);

function SessionPackaging(props: { userId: string; signedIn: boolean; onRenew: (next: SessionState) => void }) {
  const userId = props.userId;
  const transport = createTransport({ baseUrl: `${location.origin}/wamn`, cookie: true });
  const send = createSend<Record<string, PackagingCreateRequest>>({
    transport,
    bindings: { [PACKAGING_CREATE_ROUTE.operation]: { invoke: create, fields: PACKAGING_CREATE_REQUEST_FIELDS } },
    renew: async () => {
      const session = await renew(address.aud);
      props.onRenew(session ? { status: "signedIn", ...session } : { status: "signedOut" });
      return session?.userId === userId;
    },
  });
  const o = outbox({
    database: outboxDatabase(address.application, address.environment, userId),
    send: (entry, signal) => (props.signedIn ? send(entry, signal) : Promise.resolve({ ok: false, retriable: true })),
    probe: async () => props.signedIn && (await transport.probe()),
  });
  const counts = outboxStatus(o);
  const [outboxReady, setOutboxReady] = createSignal(false);
  // wfe gap wamn-46v5m: no ready promise. watch first reports empty memory, then notifies
  // after loading IndexedDB. Register before yielding to that load.
  let initialCounts = true;
  const stopReady = o.watch(() => {
    if (initialCounts) initialCounts = false;
    else setOutboxReady(true);
  });
  createEffect(
    () => ({ reload: waitingReload(), ready: outboxReady(), counts: counts() }),
    ({ reload, ready, counts }) => {
      if (reload && ready && canReload(counts)) reload();
    },
  );
  const persistence = rowsPersistence(rowsDatabase(address.application, address.environment, userId), [
    PACKAGING_SHAPE_DIGEST,
    LOCATION_SHAPE_DIGEST,
  ]);
  const selected = new URLSearchParams(location.search).get("status");
  const [filter, setFilter] = createSignal<StatusFilter>(
    selected === "available" || selected === "held" || selected === "consumed" ? selected : "all",
  );
  function ScopedPackaging(props: { filter: StatusFilter }) {
    const scope = props.filter;
    const offline = packagingOffline({
      packaging: packagingFeed(transport, Infinity),
      location: locationFeed(transport, Infinity),
      scope: packagingScope(scope),
      persistence,
      pollMs,
    });
    onCleanup(() => queueMicrotask(() => void offline.close()));
    return (
      <PackagingScreen
        collection={offline.packaging.c}
        outbox={o}
        filter={scope}
        offline={offline}
        signedIn={signedIn()}
        updateAvailable={waitingReload() !== undefined}
        shellResult={shellResult()}
      />
    );
  }
  const signedIn = () => props.signedIn;
  createEffect(
    () => props.signedIn,
    (signedIn) => {
      if (signedIn) void o.replay().catch(() => undefined);
    },
  );
  onCleanup(() => {
    stopReady();
    o.close();
  });
  return (
    <div>
      <label for="packaging-status">Packaging status</label>
      <select
        id="packaging-status"
        value={filter()}
        onChange={(event) => {
          const status = event.currentTarget.value as StatusFilter;
          const url = new URL(location.href);
          url.searchParams.set("status", status);
          history.replaceState(null, "", url);
          setFilter(status);
        }}
      >
        <option value="all">all</option>
        <option value="available">available</option>
        <option value="held">held</option>
        <option value="consumed">consumed</option>
      </select>
      <Loading>
        <Show when={filter()} keyed>
          {(status) => <ScopedPackaging filter={status} />}
        </Show>
      </Loading>
    </div>
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
    fetch: ((...args: Parameters<typeof fetch>) =>
      navigator.onLine ? fetch(...args) : Promise.reject(new Error("needs a connection"))) as typeof fetch,
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
        <Show when={!userId() && !navigator.onLine}>
          <p>needs a connection</p>
        </Show>
        <Show when={userId()} keyed>
          {(id) => <SessionPackaging userId={id} signedIn={signedIn(id)} onRenew={options.onState} />}
        </Show>
      </main>
    </Theme>
  );
}

render(() => <App />, document.getElementById("root")!);
