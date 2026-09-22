/**
 * The whole demo page.
 *
 * It signs in, builds one transport, and mounts the generated components under
 * it. There is no router, no navigation and no layout system, because the page
 * exists to judge the components and is deleted afterward.
 */

import { For, Show, createMemo, createSignal } from "solid-js";

import { createTransport, type Outcome, type Transport } from "@wamn/web-runtime";
import {
  LocationListTable,
  PurchaseOrderGetDetail,
  PurchaseOrderQueryTable,
  ReceiptGetDetail,
  PurchaseOrderUpdateForm,
  ReceiptQueryTable,
  ReceivingLoadPurchaseOrderHistoryTable,
  ReceivingLoadReceiptScreenTable,
  ReceivingRecordReceiptForm,
} from "@wamn/receiving-client/components/index.js";

import { environments, session, type Environment } from "./session.js";

export function App() {
  const [email, setEmail] = createSignal("receiving-demo@wamn.dev");
  const [password, setPassword] = createSignal("");
  const [reachable, setReachable] = createSignal<Environment[]>([]);
  const [token, setToken] = createSignal<string | null>(null);
  const [trouble, setTrouble] = createSignal<string | null>(null);
  const [outcome, setOutcome] = createSignal<string | null>(null);
  // One purchase order and one receipt feed every screen that needs a record.
  // A row link writes them, and the operator can also paste one.
  const [order, setOrder] = createSignal("");
  const [receipt, setReceipt] = createSignal("");

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

  const read = (result: Outcome<unknown>) => setOutcome(describe(result));

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
          <div>
            <p>signed in, and the token is held in memory</p>
            <Show when={outcome()}>
              <p>last outcome: {outcome()}</p>
            </Show>
            <label>
              selected purchase order
              <input
                type="text"
                size="40"
                value={order()}
                onInput={(event) => setOrder(event.currentTarget.value)}
              />
            </label>
            {screens(ready(), { order, setOrder, receipt, setReceipt }, read)}
          </div>
        )}
      </Show>
    </main>
  );
}

/** What the page holds for the screens that name one record. */
interface Picked {
  readonly order: () => string;
  readonly setOrder: (id: string) => void;
  readonly receipt: () => string;
  readonly setReceipt: (id: string) => void;
}

/** The generated screens, in the order an operator meets them. */
function screens(
  transport: Transport,
  picked: Picked,
  read: (outcome: Outcome<unknown>) => void,
) {
  const { order, setOrder, receipt, setReceipt } = picked;
  return (
    <>
      <section>
        <h2>purchase_order.query</h2>
        <PurchaseOrderQueryTable
          transport={transport}
          onRowSelect={(row) => setOrder(row.id)}
          onOpenPurchaseOrderGet={(row) => setOrder(row.id)}
          onOutcome={read}
        />
      </section>

      <section>
        <h2>purchase_order.get</h2>
        <Show when={order() !== ""} fallback={<p>pick a purchase order first</p>}>
          <PurchaseOrderGetDetail
            transport={transport}
            input={{ id: order(), requestId: "" }}
            onOutcome={read}
          />
        </Show>
      </section>

      <section>
        <h2>location.list</h2>
        <LocationListTable transport={transport} onOutcome={read} />
      </section>

      <section>
        <h2>receipt.query</h2>
        <ReceiptQueryTable
          transport={transport}
          onRowSelect={(row) => setReceipt(row.id)}
          onOpenReceiptGet={(row) => setReceipt(row.id)}
          onOutcome={read}
        />
      </section>

      <section>
        <h2>receipt.get</h2>
        <Show when={receipt() !== ""} fallback={<p>pick a receipt first</p>}>
          <ReceiptGetDetail
            transport={transport}
            input={{ id: receipt(), requestId: "" }}
            onOutcome={read}
          />
        </Show>
      </section>

      <section>
        <h2>receiving.load_receipt_screen</h2>
        <Show when={order() !== ""} fallback={<p>pick a purchase order first</p>}>
          <ReceivingLoadReceiptScreenTable
            transport={transport}
            fixed={{ purchaseOrderId: order() }}
            onOutcome={read}
          />
        </Show>
      </section>

      <section>
        <h2>purchase_order.update</h2>
        <Show when={order() !== ""} fallback={<p>pick a purchase order first</p>}>
          <PurchaseOrderUpdateForm
            transport={transport}
            key={{ id: order(), requestId: "" }}
            onSubmitted={read}
          />
        </Show>
      </section>

      <section>
        <h2>receiving.record_receipt</h2>
        {/* No initial values: `initial` is a shallow Partial, so it cannot
            carry one member of `value` without the supplied ones. wamn-m9qt. */}
        <ReceivingRecordReceiptForm transport={transport} onSubmitted={read} />
      </section>

      <section>
        <h2>receiving.load_purchase_order_history</h2>
        <Show when={order() !== ""} fallback={<p>pick a purchase order first</p>}>
          <ReceivingLoadPurchaseOrderHistoryTable
            transport={transport}
            fixed={{ id: order(), afterPosition: "0", limit: "20" }}
            onOutcome={read}
          />
        </Show>
      </section>
    </>
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
