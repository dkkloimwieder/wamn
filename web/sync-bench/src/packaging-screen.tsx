import { For, Show, createMemo, createSignal, createUniqueId, latest, onCleanup, untrack } from "solid-js";
import {
  liveRows,
  outboxStatus,
  syncCurrentAt,
  syncStatus,
  type Outbox,
  type OutboxEntry,
  type ShellResult,
} from "@wfe/sync";
import type { packagingCollection } from "./generated/wamn_wms/packaging.js";
import { orderedIds, type StatusFilter } from "./order.js";
import { packagingWrites, type PackagingDraft } from "./packaging-writes.js";
import { shapeA } from "./shape-a.js";
import { PackagingView } from "./view.js";
import { shellStatus } from "./shell-status.js";
import { needsConnection, type OfflineResult, type packagingOffline } from "./packaging-offline.js";

/** The live packaging rows, create form, and refused writes. */
export function PackagingScreen(props: {
  collection: ReturnType<typeof packagingCollection>;
  outbox: Outbox;
  signedIn?: boolean;
  updateAvailable?: boolean;
  shellResult?: ShellResult;
  filter?: StatusFilter;
  offline?: ReturnType<typeof packagingOffline>;
}) {
  const c = props.collection;
  const o = props.outbox;
  const filter = () => props.filter ?? "all";
  const source = shapeA(c, filter);
  const writes = packagingWrites(c, o);
  const current = liveRows(c);
  const counts = outboxStatus(o);
  const entries = createMemo(() => {
    counts();
    return o.entries().filter((entry) => entry.state === "refused");
  });
  const ids = createMemo(() => orderedIds(writes.rows, filter()));
  const readAt = syncCurrentAt(c);
  const currentAt = createMemo(() => {
    source.status();
    const at = readAt();
    return props.offline?.packaging.currentAt() ?? at;
  });
  const [locations, setLocations] = createSignal(props.offline?.location);
  void props.offline?.restored.then(() => setLocations(props.offline?.location));
  const [online, setOnline] = createSignal(navigator.onLine);
  const connectionChanged = () => setOnline(navigator.onLine);
  window.addEventListener("online", connectionChanged);
  window.addEventListener("offline", connectionChanged);
  onCleanup(() => {
    window.removeEventListener("online", connectionChanged);
    window.removeEventListener("offline", connectionChanged);
  });
  const missing = createMemo(() => {
    const status = source.status();
    currentAt();
    const local = props.offline
      ? props.offline.packaging.local()
      : status === "loading"
        ? undefined
        : status === "ready" || Object.keys(current).length > 0;
    return needsConnection(online(), local);
  });
  const [busy, setBusy] = createSignal(false);
  const [result, setResult] = createSignal<OfflineResult>();
  const [offlineError, setOfflineError] = createSignal("");
  return (
    <div data-shape="a" class="bench-column">
      <h2>Packaging</h2>
      <p aria-label="Sync status">
        <Show when={props.updateAvailable}>update available, reload. </Show>
        <Show when={props.signedIn === false}>Sign in to upload. </Show>
        Status: <output data-sync-status={source.status()}>{source.status()}</output>. Pending:{" "}
        {counts().pending + counts().sending}. Refused: {counts().refused}. Last sent: {counts().lastSentAt ?? "never"}.
        Current as of: {currentAt() ?? "not yet read"}. {shellStatus(props.shellResult)}
      </p>
      <Show when={missing()}>
        <p>needs a connection</p>
      </Show>
      <Show when={props.offline}>
        <button
          disabled={busy()}
          onClick={async () => {
            setBusy(true);
            setResult(undefined);
            setOfflineError("");
            try {
              setResult(await props.offline!.takeOffline());
              setLocations(props.offline?.location);
            } catch (error) {
              setOfflineError(String(error));
            } finally {
              setBusy(false);
            }
          }}
        >
          Take offline
        </button>
        <Show when={props.shellResult?.persisted === false}>
          <p>Storage is not protected.</p>
        </Show>
        <p role="status" aria-label="Take offline result">
          <Show when={busy()}>Preparing local rows.</Show>
          <Show when={result()}>
            {(value) => (
              <>
                Saved. Packaging: {value().packaging.count} rows, {value().packaging.bytes} bytes. Location:{" "}
                {value().location.count} rows, {value().location.bytes} bytes. Current as of: {value().packaging.asOf}.
                Location as of: {value().location.asOf}.
              </>
            )}
          </Show>
          {offlineError()}
        </p>
        <Show when={locations()} keyed>
          {(source) => <LocalLocations source={source} />}
        </Show>
      </Show>
      <Show when={source.status() === "ready" || currentAt() !== undefined || Object.keys(current).length > 0}>
        <CreatePackaging create={writes.create} locations={locations()?.c} />
        <PackagingView
          label="Packaging"
          source={{ ...source, ids, get: (id) => writes.rows[id] }}
          tentative={(id) => writes.rows[id]?.rowVersion === undefined}
        />
      </Show>
      <section aria-label="Not uploaded">
        <h2>Not uploaded</h2>
        <For each={entries()} keyed={(entry) => entry.seq}>
          {(entry) => (
            <RefusedPackaging
              entry={entry()}
              current={() => current[entry().expect.id]}
              resubmit={(edited) => writes.resubmit(entry(), edited)}
              discard={() => o.discard(entry().seq)}
            />
          )}
        </For>
      </section>
    </div>
  );
}

type Locations = NonNullable<ReturnType<typeof packagingOffline>["location"]>["c"];

function LocalLocations(props: { source: NonNullable<ReturnType<typeof packagingOffline>["location"]> }) {
  const rows = liveRows(props.source.c);
  const readAt = syncCurrentAt(props.source.c);
  const status = syncStatus(props.source.c);
  const currentAt = createMemo(() => {
    status();
    const at = readAt();
    return props.source.currentAt() ?? at;
  });
  return (
    <Show when={currentAt()}>
      <p aria-label="Local locations">
        Location: {Object.keys(rows).length} rows. Current as of: {currentAt()}.
      </p>
    </Show>
  );
}

function LocationPicker(props: { collection: Locations }) {
  const id = createUniqueId();
  const rows = liveRows(props.collection);
  const ids = createMemo(() =>
    Object.keys(rows).sort((a, b) => rows[a]!.locationCode.localeCompare(rows[b]!.locationCode)),
  );
  return (
    <>
      <label for={id}>Location</label>
      <select id={id} name="locationId" required>
        <option value="">Select a location</option>
        <For each={ids()}>{(id) => <option value={id}>{rows[id]!.locationCode}</option>}</For>
      </select>
    </>
  );
}

function CreatePackaging(props: { create: (request: PackagingDraft) => Promise<void>; locations?: Locations }) {
  const [busy, setBusy] = createSignal(false);
  const [message, setMessage] = createSignal("");
  return (
    <form
      aria-label="Create packaging"
      class="bench-controls"
      onSubmit={async (event) => {
        event.preventDefault();
        const form = event.currentTarget;
        const values = new FormData(form);
        setBusy(true);
        setMessage("");
        try {
          await props.create({
            packagingCode: String(values.get("packagingCode")),
            locationId: String(values.get("locationId")),
            type: String(values.get("type")) as PackagingDraft["type"],
            status: String(values.get("status")) as PackagingDraft["status"],
          });
          form.reset();
          setMessage("Recorded");
        } catch (error) {
          setMessage(String(error));
        } finally {
          setBusy(false);
        }
      }}
    >
      <label>
        Packaging code <input name="packagingCode" required />
      </label>
      <Show
        when={props.locations}
        keyed
        fallback={
          <label>
            Location id <input name="locationId" required />
          </label>
        }
      >
        {(collection) => <LocationPicker collection={collection} />}
      </Show>
      <label>
        Type{" "}
        <select name="type">
          <For each={["bin", "case", "loose", "pallet", "tote"]}>{(type) => <option>{type}</option>}</For>
        </select>
      </label>
      <label>
        Status{" "}
        <select name="status">
          <option>available</option>
          <option>held</option>
        </select>
      </label>
      {/* Recording status must render while recordWrite still waits for upload and the feed. */}
      <button type="submit" disabled={latest(busy)}>
        Create packaging
      </button>
      <output>{latest(message)}</output>
    </form>
  );
}

function RefusedPackaging(props: {
  entry: OutboxEntry;
  current: () => unknown;
  resubmit: (edited: string) => Promise<void>;
  discard: () => Promise<void>;
}) {
  const [edited, setEdited] = createSignal(untrack(() => JSON.stringify(props.entry.request, null, 2)));
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal("");
  async function run(action: () => Promise<void>) {
    setBusy(true);
    setError("");
    try {
      await action();
    } catch (cause) {
      setError(String(cause));
    } finally {
      setBusy(false);
    }
  }
  return (
    <article data-entry={props.entry.seq}>
      <p>
        Operation: {props.entry.operation}. Record id: {props.entry.expect.id}
      </p>
      <p>{props.entry.refusal?.text ?? "Refused"}</p>
      <div class="bench-shapes">
        <div>
          <h3>Stored request</h3>
          <pre>{JSON.stringify(props.entry.request, null, 2)}</pre>
          <label>
            Corrected request{" "}
            <textarea rows={10} value={edited()} onInput={(event) => setEdited(event.currentTarget.value)} />
          </label>
        </div>
        <div>
          <h3>Current row</h3>
          <pre>{props.current() === undefined ? "no row" : JSON.stringify(props.current(), null, 2)}</pre>
        </div>
      </div>
      <button disabled={busy()} onClick={() => void run(() => props.resubmit(edited()))}>
        Resubmit
      </button>
      <button disabled={busy()} onClick={() => void run(props.discard)}>
        Discard
      </button>
      <p role="alert">{error()}</p>
    </article>
  );
}
