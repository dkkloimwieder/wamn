/**
 * The shell on a stub identity service.
 *
 * Each test sets the address, renders the shell with two stub screens, and
 * reads what an operator sees. The stub answers the four password paths the
 * way the identity service does for a cookie session.
 */

import { cleanup, fireEvent, render, screen } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { ColorModeProvider } from "@wamn/ui";

import { createSignal } from "solid-js";

import { Shell, type ScreenProps, type ShellSection } from "../src/index.js";

const AUD = "urn:wamn:project-env:acme:widgets:dev:k3m9x2p7";

/**
 * A screen that shows its name, and a button that reads the transport. A
 * generated table reads its transport the same way, in a click handler after
 * the screen rendered.
 */
function stubScreen(name: string) {
  return (props: ScreenProps) => {
    const [used, setUsed] = createSignal(false);
    return (
      <>
        <p>{name} screen</p>
        <button type="button" onClick={() => setUsed(typeof props.transport.invoke === "function")}>
          use {name}
        </button>
        <p>{used() ? `${name} holds a transport` : ""}</p>
      </>
    );
  };
}

const SECTIONS: readonly ShellSection[] = [
  { label: "Pallets", screens: [{ path: "pallets", label: "pallet query", component: stubScreen("pallets") }] },
  { label: "Products", screens: [{ path: "products", label: "product query", component: stubScreen("products") }] },
];

/** The identity service, holding one session cookie or none. */
function identity(signedIn: boolean) {
  const state = { signedIn, calls: [] as string[] };
  const times = () =>
    new Response(
      JSON.stringify({ expires_at: Date.now() / 1000 + 3600, login_expires_at: Date.now() / 1000 + 86400 }),
      { status: 200 },
    );
  const fetch = (url: string | URL | Request) => {
    const path = String(url);
    state.calls.push(path);
    switch (path) {
      case "/password/environments":
        return Promise.resolve(
          new Response(
            JSON.stringify({ environments: [{ aud: AUD, org: "acme", project: "widgets", env: "dev" }] }),
            { status: 200 },
          ),
        );
      case "/password/session":
        state.signedIn = true;
        return Promise.resolve(times());
      case "/password/renew":
        return Promise.resolve(state.signedIn ? times() : new Response("", { status: 401 }));
      case "/password/logout":
        state.signedIn = false;
        return Promise.resolve(new Response("", { status: 204 }));
      default:
        return Promise.resolve(new Response("", { status: 404 }));
    }
  };
  return { state, fetch: fetch as typeof globalThis.fetch };
}

function open(path: string, fetch: typeof globalThis.fetch) {
  window.history.pushState({}, "", path);
  render(() => (
    <ColorModeProvider initialColorMode="light">
      <Shell title="Stub app" sections={SECTIONS} fetch={fetch} />
    </ColorModeProvider>
  ));
}

/** Signs in once the form shows, which is after the renewal answers. */
async function signIn() {
  fireEvent.input(await screen.findByLabelText("email"), { target: { value: "someone@wamn.dev" } });
  fireEvent.input(screen.getByLabelText("password"), { target: { value: "a long password string" } });
  fireEvent.click(screen.getByText("sign in"));
}

beforeEach(() => {
  // jsdom has no media queries, and the sidebar asks whether the page is narrow.
  window.matchMedia = ((query: string) => ({
    matches: false,
    media: query,
    addEventListener: () => undefined,
    removeEventListener: () => undefined,
  })) as unknown as typeof window.matchMedia;
});

afterEach(cleanup);

describe("the app shell", () => {
  it("signs in at the root, and the chosen environment opens the first screen", async () => {
    const { fetch } = identity(false);
    open("/", fetch);
    await signIn();
    fireEvent.click(await screen.findByText("acme/widgets/dev"));
    expect(await screen.findByText("pallets screen")).toBeDefined();
    expect(window.location.pathname).toBe(`/${AUD}/pallets`);
  });

  it("asks for the password on a screen address with no session, and shows that screen after it", async () => {
    const { fetch } = identity(false);
    open(`/${AUD}/products`, fetch);
    await signIn();
    expect(await screen.findByText("products screen")).toBeDefined();
    expect(window.location.pathname).toBe(`/${AUD}/products`);
  });

  it("hands a screen the transport it reads after it rendered", async () => {
    const { fetch } = identity(true);
    open(`/${AUD}/pallets`, fetch);
    fireEvent.click(await screen.findByText("use pallets"));
    expect(await screen.findByText("pallets holds a transport")).toBeDefined();
  });

  it("renews the session on a reload, with no sign in", async () => {
    const { state, fetch } = identity(true);
    open(`/${AUD}/products`, fetch);
    expect(await screen.findByText("products screen")).toBeDefined();
    expect(screen.queryByLabelText("password")).toBeNull();
    expect(state.calls).toEqual(["/password/renew"]);
  });

  it("signs out, and asks for the password on the same address", async () => {
    const { state, fetch } = identity(true);
    open(`/${AUD}/pallets`, fetch);
    await screen.findByText("pallets screen");
    fireEvent.click(screen.getByText("sign out"));
    expect(await screen.findByLabelText("password")).toBeDefined();
    expect(screen.queryByText("pallets screen")).toBeNull();
    expect(state.signedIn).toBe(false);
    expect(window.location.pathname).toBe(`/${AUD}/pallets`);
  });

  it("lists every screen by section, and a navigation entry opens its screen", async () => {
    const { fetch } = identity(true);
    open(`/${AUD}/pallets`, fetch);
    await screen.findByText("pallets screen");
    expect(screen.getByText("Pallets")).toBeDefined();
    expect(screen.getByText("Products")).toBeDefined();
    expect(screen.getByText("pallet query").closest("a")?.getAttribute("data-active")).toBe("true");
    fireEvent.click(screen.getByText("product query"));
    expect(await screen.findByText("products screen")).toBeDefined();
    expect(window.location.pathname).toBe(`/${AUD}/products`);
    expect(screen.getByText("product query").closest("a")?.getAttribute("data-active")).toBe("true");
    expect(screen.getByText("pallet query").closest("a")?.getAttribute("data-active")).toBeNull();
  });

  it("shows no page for an address that names none", async () => {
    const { fetch } = identity(true);
    open(`/${AUD}/nothing/here`, fetch);
    expect(await screen.findByText("This address names no page.")).toBeDefined();
  });
});
