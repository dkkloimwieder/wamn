/**
 * The whole demo page.
 *
 * It signs in by cookie, builds one transport, and mounts the generated
 * components under it. There is no router, no navigation and no layout system,
 * because the page exists to judge the components and is deleted afterward.
 */

import { For, Show, createMemo, createSignal, type JSX } from "solid-js";

import {
  Button,
  Card,
  CardContent,
  CardDescription,
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
  type Transport,
} from "@wamn/web-runtime";
import {
  LocationListTable,
  LocationListTableLabel,
  PurchaseOrderGetDetail,
  PurchaseOrderGetDetailLabel,
  PurchaseOrderQueryTable,
  PurchaseOrderQueryTableLabel,
  PurchaseOrderUpdateForm,
  PurchaseOrderUpdateFormLabel,
  ReceiptGetDetail,
  ReceiptGetDetailLabel,
  ReceiptQueryTable,
  ReceiptQueryTableLabel,
  ReceivingLoadPurchaseOrderHistoryTable,
  ReceivingLoadPurchaseOrderHistoryTableLabel,
  ReceivingLoadReceiptScreenTable,
  ReceivingLoadReceiptScreenTableLabel,
  ReceivingRecordReceiptForm,
  ReceivingRecordReceiptFormLabel,
} from "@wamn/receiving-client/components/index.js";

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
  const [email, setEmail] = createSignal("receiving-demo@wamn.dev");
  const [password, setPassword] = createSignal("");
  const [reachable, setReachable] = createSignal<Environment[]>([]);
  const [signedIn, setSignedIn] = createSignal(false);
  const [trouble, setTrouble] = createSignal<string | null>(null);
  const [outcome, setOutcome] = createSignal<string | null>(null);
  // One purchase order and one receipt feed every screen that needs a record.
  // A row link writes them, and the operator can also paste one.
  const [order, setOrder] = createSignal("");
  const [receipt, setReceipt] = createSignal("");

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
            <h1 class="text-base font-semibold uppercase">Receiving demo</h1>
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
              <Card size="sm">
                <CardContent class="flex flex-wrap items-end gap-6">
                  <Field class="max-w-md">
                    <FieldLabel for="demo-order">selected purchase order</FieldLabel>
                    <Input
                      id="demo-order"
                      type="text"
                      placeholder="choose a row in Purchase orders"
                      value={order()}
                      onInput={(event) => setOrder(event.currentTarget.value)}
                    />
                  </Field>
                  <div class="flex flex-col gap-1 pb-2">
                    <span class="text-xs uppercase text-muted-foreground">selected receipt</span>
                    <span class="font-mono text-sm">{receipt() === "" ? "none" : receipt()}</span>
                  </div>
                </CardContent>
              </Card>
              {screens(ready(), { order, setOrder, receipt, setReceipt }, read)}
            </div>
          )}
        </Show>
      </main>
    </div>
  );
}

/** One screen on the page: a card with its title and the operation it calls. */
function Panel(props: { title: string; operation: string; children: JSX.Element }) {
  return (
    <Card class="min-w-0">
      <CardHeader>
        <CardTitle>
          <h2>{props.title}</h2>
        </CardTitle>
        <CardDescription class="font-mono">{props.operation}</CardDescription>
      </CardHeader>
      <CardContent class="min-w-0">{props.children}</CardContent>
    </Card>
  );
}

/** What a screen shows until the record it needs is chosen. */
function Waiting(props: { children: JSX.Element }) {
  return (
    <p class="border border-dashed p-8 text-center text-sm text-muted-foreground">
      {props.children}
    </p>
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
function screens(transport: Transport, picked: Picked, read: (outcome: Outcome<unknown>) => void) {
  const { order, setOrder, receipt, setReceipt } = picked;
  const needsOrder = "Choose a purchase order in the table above.";
  return (
    <>
      <Panel title={PurchaseOrderQueryTableLabel} operation="purchase_order.query">
        <PurchaseOrderQueryTable
          transport={transport}
          onRowSelect={(row) => setOrder(row.id)}
          onOpenPurchaseOrderGet={(row) => setOrder(row.id)}
          onOutcome={read}
        />
      </Panel>

      <div class="grid gap-6 xl:grid-cols-2">
        <Panel title={PurchaseOrderGetDetailLabel} operation="purchase_order.get">
          <Show when={order() !== ""} fallback={<Waiting>{needsOrder}</Waiting>}>
            <PurchaseOrderGetDetail
              transport={transport}
              input={{ id: order() }}
              onOutcome={read}
            />
          </Show>
        </Panel>

        <Panel title={PurchaseOrderUpdateFormLabel} operation="purchase_order.update">
          <Show when={order() !== ""} fallback={<Waiting>{needsOrder}</Waiting>}>
            <PurchaseOrderUpdateForm
              transport={transport}
              key={{ id: order() }}
              onSubmitted={read}
            />
          </Show>
        </Panel>
      </div>

      <Panel title={ReceivingRecordReceiptFormLabel} operation="receiving.record_receipt">
        <Show when={order() !== ""} fallback={<Waiting>{needsOrder}</Waiting>}>
          <ReceivingRecordReceiptForm
            transport={transport}
            initial={{ value: { purchaseOrderId: order() } }}
            onSubmitted={read}
          />
        </Show>
      </Panel>

      <Panel title={ReceivingLoadReceiptScreenTableLabel} operation="receiving.load_receipt_screen">
        <Show when={order() !== ""} fallback={<Waiting>{needsOrder}</Waiting>}>
          <ReceivingLoadReceiptScreenTable
            transport={transport}
            fixed={{ purchaseOrderId: order() }}
            onOutcome={read}
          />
        </Show>
      </Panel>

      <Panel
        title={ReceivingLoadPurchaseOrderHistoryTableLabel}
        operation="receiving.load_purchase_order_history"
      >
        <Show when={order() !== ""} fallback={<Waiting>{needsOrder}</Waiting>}>
          <ReceivingLoadPurchaseOrderHistoryTable
            transport={transport}
            fixed={{ id: order(), limit: 20 }}
            onOutcome={read}
          />
        </Show>
      </Panel>

      <div class="grid gap-6 xl:grid-cols-2">
        <Panel title={ReceiptQueryTableLabel} operation="receipt.query">
          <ReceiptQueryTable
            transport={transport}
            onRowSelect={(row) => setReceipt(row.id)}
            onOpenReceiptGet={(row) => setReceipt(row.id)}
            onOutcome={read}
          />
        </Panel>

        <Panel title={ReceiptGetDetailLabel} operation="receipt.get">
          <Show
            when={receipt() !== ""}
            fallback={<Waiting>Choose a receipt in the table beside this one.</Waiting>}
          >
            <ReceiptGetDetail transport={transport} input={{ id: receipt() }} onOutcome={read} />
          </Show>
        </Panel>
      </div>

      <Panel title={LocationListTableLabel} operation="location.list">
        <LocationListTable transport={transport} onOutcome={read} />
      </Panel>
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
