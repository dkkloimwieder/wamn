/**
 * The two receipt screens on sync.
 *
 * Each screen reads its rows from the generated collection and records a
 * receipt through the generated write helper and the signed-in user's outbox.
 * The tentative row is the one line that the row action records.
 */
import { createMemo, createSignal, For, onCleanup, Show } from "solid-js";
import { Button, Field, FieldError, Input } from "@wfe/components";
import {
  liveRows,
  outbox,
  shell,
  syncCurrentAt,
  syncStatus,
  type OperationCall,
  type Outbox,
  type OutboxEntry,
  type ShellResult,
  type SyncCollection,
} from "@wfe/sync";
import {
  createSend,
  occurredAt,
  outboxDatabase,
  renew,
  requestHash,
  rowsDatabase,
  rowsPersistence,
  type Transport,
} from "@wamn/web-runtime";
import { Page, useSession, type ScreenProps } from "@wamn/platform-ui";
import { NotUploaded, SyncStatusLine } from "../../../../web/shell/src/sync.js";
import { LOCATION_LIST_ROUTE } from "./generated/wamn_receiving/location.js";
import * as receiving from "./generated/wamn_receiving/receiving.js";
import type { ScreenPresentation } from "./generated/wamn_receiving/components/index.js";
import type { TableDescription } from "@wfe/generator";
import { Records } from "./table";
import { invoke, rowsOf, type RecordValue, type Rows } from "./data";

type Line = receiving.ReceivingLoadOpenReceiptLinesRow;
type Tentative = Line & { $tentative?: boolean };
type ReceiptRequest = Omit<receiving.ReceivingRecordReceiptRequest, "requestId" | "value"> & {
  value: Omit<receiving.ReceivingRecordReceiptRequestValue, "idempotencyKey">;
};

const [shellResult, setShellResult] = createSignal<ShellResult>();
const [waitingReload, setWaitingReload] = createSignal<() => void>();

/** Register the worker and ask for persistent storage, once per page. */
export function startShell(): void {
  void shell({ workerUrl: "/sw.js", onUpdate: (reload) => setWaitingReload(() => reload) }).then(setShellResult);
}

/** The application route carries its audience in the first path segment. */
function receivingAddress(path: string): { aud: string; application: string; environment: string } {
  const aud = decodeURIComponent(path.split("/")[1] ?? "");
  const match = /^urn:wamn:project-env:([^:]+):([^:]+):([^:]+):([^:]+)$/.exec(aud);
  if (!match) throw new Error("Open Receiving at /urn:wamn:project-env:<org>:<project>:<env>:<instance>/.");
  return { aud, application: match[2]!, environment: match[3]! };
}

/** One user's outbox and saved rows. */
interface Store {
  readonly userId: string;
  readonly outbox: Outbox;
  readonly persistence: ReturnType<typeof rowsPersistence>;
  /** The locations last read while online, for a receipt recorded offline. */
  readonly locations: () => readonly RecordValue[] | undefined;
}

let current: Store | undefined;

/** The store of the signed-in user. A different user closes the old user's outbox. */
function storeFor(userId: string, transport: Transport): Store {
  if (current?.userId === userId) return current;
  current?.outbox.close();
  const address = receivingAddress(location.pathname);
  const send = createSend<Record<string, receiving.ReceivingRecordReceiptRequest>>({
    transport,
    bindings: {
      [receiving.RECEIVING_RECORD_RECEIPT_ROUTE.operation]: {
        invoke: receiving.recordReceipt,
        fields: receiving.RECEIVING_RECORD_RECEIPT_REQUEST_FIELDS,
      },
    },
    renew: async () => (await renew(address.aud))?.userId === userId,
  });
  const o = outbox({
    database: outboxDatabase(address.application, address.environment, userId),
    send,
    probe: async () => (await transport.probe?.()) ?? navigator.onLine,
  });
  const [locations, setLocations] = createSignal<readonly RecordValue[]>();
  void invoke<Rows>(transport, LOCATION_LIST_ROUTE, {}).then(
    (result) => setLocations(rowsOf(result)),
    () => undefined,
  );
  current = {
    userId,
    outbox: o,
    persistence: rowsPersistence(rowsDatabase(address.application, address.environment, userId), [
      receiving.RECEIVING_SHAPE_DIGEST,
    ]),
    locations,
  };
  void o.replay().catch(() => undefined);
  return current;
}

/** The wire row a table description names, from the collection row. */
function wireRow(row: Tentative): RecordValue {
  const fields = receiving.RECEIVING_LOAD_OPEN_RECEIPT_LINES_RESULT_FIELDS["item"];
  const members = typeof fields === "object" ? fields.fields : {};
  const wire: RecordValue = {};
  for (const [key, member] of Object.entries(members)) {
    if (typeof member === "string") wire[key] = row[member as keyof Line] as RecordValue[string];
  }
  return wire;
}

/** Exact decimal sum of two numeric texts, with the larger scale. */
export function addDecimal(left: string, right: string): string {
  const parse = (text: string) => {
    const negative = text.startsWith("-");
    const [whole = "0", fraction = ""] = text.replace(/^[-+]/, "").split(".");
    return { units: BigInt(whole + fraction) * (negative ? -1n : 1n), scale: fraction.length };
  };
  const a = parse(left);
  const b = parse(right);
  const scale = Math.max(a.scale, b.scale);
  const sum = a.units * 10n ** BigInt(scale - a.scale) + b.units * 10n ** BigInt(scale - b.scale);
  const sign = sum < 0n ? "-" : "";
  const digits = (sum < 0n ? -sum : sum).toString().padStart(scale + 1, "0");
  return scale === 0 ? `${sign}${digits}` : `${sign}${digits.slice(0, -scale)}.${digits.slice(-scale)}`;
}

/** The line as the receipt leaves it, shown until a complete pass brings the host's row. */
export function receivedLine(line: Line, quantity: string): Partial<Tentative> {
  return {
    lineId: line.lineId,
    receivedQuantity: addDecimal(line.receivedQuantity, quantity),
    remainingQuantity: addDecimal(line.remainingQuantity, `-${quantity}`),
    $tentative: true,
  };
}

/** One record_receipt call for one line, keyed to that line in the outbox. */
export async function receiptCall(request: ReceiptRequest): Promise<OperationCall> {
  return {
    operation: receiving.RECEIVING_RECORD_RECEIPT_ROUTE.operation,
    request,
    request_hash: await requestHash(request),
    expect: { model: "purchase_order_line", id: request.value.line[0]!.purchaseOrderLineId },
    append_only: false,
  };
}

/** One receipt screen: the table over the collection, the receipt panel, the status line and the not uploaded panel. */
export function SyncReceipts(props: {
  screen: ScreenProps;
  description: TableDescription & { readonly presentation: ScreenPresentation };
  /** The one-order screen's purchase order, or undefined for every open line. */
  purchaseOrderId?: string;
}) {
  const session = useSession();
  if (session === undefined) throw new Error("a sync screen needs a signed-in shell");
  const store = storeFor(session.userId, props.screen.transport);
  const options = { persistence: store.persistence };
  const c: SyncCollection<Line> =
    props.purchaseOrderId === undefined
      ? receiving.loadOpenReceiptLinesCollection(
          receiving.loadOpenReceiptLinesFeed(props.screen.transport),
          {},
          options,
        )
      : (receiving.loadReceiptScreenCollection(
          receiving.loadReceiptScreenFeed(props.screen.transport),
          { purchaseOrderId: props.purchaseOrderId },
          options,
        ) as SyncCollection<Line>);
  const record =
    props.purchaseOrderId === undefined
      ? receiving.loadOpenReceiptLinesWrite(c, store.outbox)
      : receiving.loadReceiptScreenWrite(c, store.outbox);
  const rows = record.rows as Record<string, Tentative>;
  const base = liveRows(c);
  const status = syncStatus(c);
  const currentAt = syncCurrentAt(c);
  // The overlay can restore entries of lines outside this scope, so the scope's own rows decide what shows.
  const shown = createMemo(() =>
    Object.keys(base)
      .map((id) => rows[id] ?? base[id]!)
      .sort((a, b) => a.purchaseOrderNumber.localeCompare(b.purchaseOrderNumber) || a.lineNumber - b.lineNumber),
  );
  const [selected, setSelected] = createSignal<Line>();

  async function write(request: ReceiptRequest, line: Line): Promise<void> {
    const call = await receiptCall(request);
    // The watch sees the committed entry. The write itself still waits for a complete pass.
    await new Promise<void>((resolve, reject) => {
      const stop = store.outbox.watch(() => {
        if (store.outbox.entries().some((entry) => entry.request_hash === call.request_hash)) {
          stop();
          resolve();
        }
      });
      void record.write(call, receivedLine(line, request.value.line[0]!.quantity)).catch((error: unknown) => {
        // A refusal reverts the overlay. The not uploaded panel owns it.
        stop();
        reject(error);
      });
    });
  }

  async function resubmit(entry: OutboxEntry, edited: string): Promise<void> {
    const value: unknown = JSON.parse(edited);
    if (value === null || typeof value !== "object" || Array.isArray(value)) {
      throw new Error("The request must be a JSON object.");
    }
    const request = value as ReceiptRequest;
    if (request.value?.line?.[0]?.purchaseOrderLineId !== entry.expect.id) {
      throw new Error("The corrected receipt must keep its order line.");
    }
    const line = base[entry.expect.id];
    if (line === undefined) throw new Error("This screen does not show the receipt's order line.");
    const call = await receiptCall(request);
    if (call.request_hash === entry.request_hash) throw new Error("Edit the request before resubmitting it.");
    await store.outbox.discard(entry.seq);
    await write(request, line);
  }

  return (
    <Page title={props.description.title}>
      <SyncStatusLine
        outbox={store.outbox}
        offline={session.offline()}
        status={status()}
        currentAt={currentAt()}
        updateAvailable={waitingReload() !== undefined}
        shellResult={shellResult()}
      />
      <Records
        screen={props.screen}
        description={props.description}
        route={
          props.purchaseOrderId === undefined
            ? receiving.RECEIVING_LOAD_OPEN_RECEIPT_LINES_ROUTE
            : receiving.RECEIVING_LOAD_RECEIPT_SCREEN_ROUTE
        }
        rows={() => shown().map(wireRow)}
        tentative={(row) => rows[String(row.line_id)]?.$tentative === true}
        rowCommand={{
          label: "Record a receipt",
          run: (row) => setSelected(base[String(row.line_id)]),
        }}
      />
      <Show when={selected()} keyed>
        {(line) => (
          <ReceiptPanel
            line={line}
            locations={store.locations()}
            record={(request) => write(request, line)}
            close={() => setSelected(undefined)}
          />
        )}
      </Show>
      <NotUploaded outbox={store.outbox} current={(entry) => rows[String(entry.expect.id)]} resubmit={resubmit} />
    </Page>
  );
}

function ReceiptPanel(props: {
  line: Line;
  locations: readonly RecordValue[] | undefined;
  record: (request: ReceiptRequest) => Promise<void>;
  close: () => void;
}) {
  const [reference, setReference] = createSignal("");
  const [quantity, setQuantity] = createSignal("");
  const [locationId, setLocationId] = createSignal("");
  const [message, setMessage] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  let active = true;
  onCleanup(() => {
    active = false;
  });
  return (
    <form
      aria-label="Record a receipt"
      onSubmit={async (event) => {
        event.preventDefault();
        if (!/^(?:\d+(?:\.\d*)?|\.\d+)$/.test(quantity()) || !/[1-9]/.test(quantity())) {
          setMessage("Enter a quantity greater than zero.");
          return;
        }
        setBusy(true);
        setMessage("");
        try {
          await props.record({
            value: {
              purchaseOrderId: props.line.purchaseOrderId,
              receiptReference: reference(),
              occurredAt: occurredAt(),
              line: [{ purchaseOrderLineId: props.line.lineId, locationId: locationId(), quantity: quantity() }],
            },
          });
          if (active) props.close();
        } catch (error) {
          if (active) setMessage(String(error));
        } finally {
          if (active) setBusy(false);
        }
      }}
    >
      <h2>
        Record a receipt for line {props.line.lineNumber}, {props.line.itemNumber}
      </h2>
      <Field>
        <label>
          Receipt reference <Input required value={reference()} onValueChange={setReference} />
        </label>
      </Field>
      <Field>
        <label>
          Quantity <Input required value={quantity()} onValueChange={setQuantity} />
        </label>
      </Field>
      <Field>
        <Show
          when={props.locations}
          fallback={
            <label>
              Location id <Input required value={locationId()} onValueChange={setLocationId} />
            </label>
          }
        >
          {(choices) => (
            <label>
              Location{" "}
              <select required value={locationId()} onChange={(event) => setLocationId(event.currentTarget.value)}>
                <option value="">Select a location</option>
                <For each={choices()}>
                  {(choice) => <option value={String(choice.id)}>{String(choice.location_code ?? choice.id)}</option>}
                </For>
              </select>
            </label>
          )}
        </Show>
      </Field>
      <Button type="submit" disabled={busy()}>
        Record a receipt
      </Button>
      <Button type="button" variant="ghost" onClick={props.close}>
        Cancel
      </Button>
      <Show when={message()}>{(text) => <FieldError role="alert">{text()}</FieldError>}</Show>
    </form>
  );
}
