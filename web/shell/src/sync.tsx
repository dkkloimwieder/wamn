/**
 * The sync status line and the not uploaded panel that every sync screen shows.
 *
 * Both read one outbox. The application supplies the outbox of the signed-in
 * user, and a resubmit that keeps the refused entry's record id.
 */

import { For, Show, createMemo, createSignal, untrack } from "solid-js";
import { outboxStatus, type Outbox, type OutboxEntry, type ShellResult } from "@wfe/sync";
import { shellStatus } from "./status.js";

export { shellStatus };

/** Pending, refused, last sent and current as of, for one outbox and one collection. */
export function SyncStatusLine(props: {
  outbox: Outbox;
  /** The collection's sync status. */
  status: string;
  /** When the screen's rows were last current, or undefined before the first read. */
  currentAt: string | undefined;
  signedIn?: boolean;
  /** True while the page shows the saved user, before the session is renewed. */
  offline?: boolean;
  updateAvailable?: boolean;
  shellResult?: ShellResult | undefined;
}) {
  const counts = outboxStatus(props.outbox);
  return (
    <p aria-label="Sync status">
      <Show when={props.updateAvailable}>update available, reload. </Show>
      <Show when={props.offline}>Offline, sign in to upload. </Show>
      <Show when={props.signedIn === false}>Sign in to upload. </Show>
      Status: <output data-sync-status={props.status}>{props.status}</output>. Pending:{" "}
      {counts().pending + counts().sending}. Refused: {counts().refused}. Expired: {counts().expired}. Last sent:{" "}
      {counts().lastSentAt ?? "never"}. Current as of: {props.currentAt ?? "not yet read"}.{" "}
      {shellStatus(props.shellResult)}
    </p>
  );
}

/**
 * Every refused and expired entry of the outbox. A refused entry shows its
 * stored request and a corrected one to resubmit. An expired entry shows when
 * it was recorded, and resubmits as it is with a fresh age.
 */
export function NotUploaded(props: {
  outbox: Outbox;
  /** The current row of the entry's record, or undefined when the screen holds none. */
  current: (entry: OutboxEntry) => unknown;
  /** Send the corrected request under the same record id. */
  resubmit: (entry: OutboxEntry, edited: string) => Promise<void>;
}) {
  const counts = outboxStatus(props.outbox);
  const entries = createMemo(() => {
    counts();
    return props.outbox.entries().filter((entry) => entry.state === "refused" || entry.state === "expired");
  });
  return (
    <section aria-label="Not uploaded">
      <h2>Not uploaded</h2>
      <For each={entries()} keyed={(entry) => `${entry.seq}:${entry.state}`}>
        {(entry) => (
          <Show
            when={entry().state === "expired"}
            fallback={
              <RefusedEntry
                entry={entry()}
                current={() => props.current(entry())}
                resubmit={(edited) => props.resubmit(entry(), edited)}
                discard={() => props.outbox.discard(entry().seq)}
              />
            }
          >
            <ExpiredEntry
              entry={entry()}
              resubmit={() => props.outbox.resubmit(entry().seq)}
              discard={() => props.outbox.discard(entry().seq)}
            />
          </Show>
        )}
      </For>
    </section>
  );
}

function ExpiredEntry(props: { entry: OutboxEntry; resubmit: () => Promise<void>; discard: () => Promise<void> }) {
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
    <article data-entry={props.entry.seq} data-state="expired">
      <p>
        Operation: {props.entry.operation}. Record id: {props.entry.expect.id}
      </p>
      <p>
        Expired. Recorded at <time datetime={props.entry.recorded_at}>{props.entry.recorded_at}</time>.
      </p>
      <h3>Stored request</h3>
      <pre>{JSON.stringify(props.entry.request, null, 2)}</pre>
      <button disabled={busy()} onClick={() => void run(props.resubmit)}>
        Resubmit
      </button>
      <button disabled={busy()} onClick={() => void run(props.discard)}>
        Discard
      </button>
      <p role="alert">{error()}</p>
    </article>
  );
}

function RefusedEntry(props: {
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
      <div
        style={{
          display: "grid",
          "grid-template-columns": "repeat(auto-fit, minmax(min(100%, 32rem), 1fr))",
          gap: "var(--ui-gap, 6px)",
        }}
      >
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
