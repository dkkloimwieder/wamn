/**
 * The shell on a stub identity service.
 *
 * Each test sets the address, renders the shell with three stub screens, and
 * reads what an operator sees. The stub answers the four password paths the
 * way the identity service does for a cookie session, and `permission.mine`
 * the way the application host does.
 */

import { cleanup, fireEvent, render, screen } from "@solidjs/testing-library";
import { afterEach, beforeEach } from "vitest";

import { member, user } from "../src/generated/wamn_control_org/index.js";
import { permission } from "../src/generated/wamn_control/index.js";

import { createSignal, flush, For } from "solid-js";

import { Shell, RecordLink, type ScreenProps, type ShellSection } from "../src/index.js";

export const AUD = "urn:wamn:project-env:acme:widgets:dev:k3m9x2p7";

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
        <button type="button" onClick={() => props.open("pallets/abc")}>
          open pallet abc
        </button>
      </>
    );
  };
}

/** A form page that shows its key and query values, and closes. */
function stubForm(props: ScreenProps) {
  return (
    <>
      <p>
        update {props.params.id} with {props.search.note ?? "no note"}
      </p>
      <button type="button" onClick={() => props.close()}>
        done
      </button>
    </>
  );
}

/** A record page that shows the key from its address, and opens another record. */
function stubRecord(props: ScreenProps) {
  return (
    <>
      <p>pallet {props.params.id} record</p>
      <For each={props.actions}>
        {(action) => (
          <RecordLink screen={props} path={action.path}>
            {action.label}
          </RecordLink>
        )}
      </For>
      <button type="button" onClick={() => props.open("pallets/def")}>
        open pallet def
      </button>
    </>
  );
}

const SECTIONS: readonly ShellSection[] = [
  /* Application screens remain stubs until M3 and M4. */
  {
    label: "Pallets",
    screens: [
      { path: "pallets", operation: "acme:pallet/query@1.0.0", label: "query", component: stubScreen("pallets") },
    ],
    routes: [
      {
        path: "pallets/:id",
        operation: "acme:pallet/get@1.0.0",
        component: stubRecord,
        actions: [{ label: "update", path: "pallets/:id/update?note=hi", operation: "acme:pallet/update@1.0.0" }],
      },
      { path: "pallets/:id/update", operation: "acme:pallet/update@1.0.0", component: stubForm },
    ],
  },
  {
    label: "Products",
    screens: [
      { path: "products", operation: "acme:product/query@1.0.0", label: "query", component: stubScreen("products") },
      {
        path: "product-totals",
        operation: "acme:product/aggregate@1.0.0",
        label: "aggregate",
        component: stubScreen("product totals"),
      },
    ],
  },
];

const ADMINISTRATION: ShellSection = {
  label: "Administration",
  screens: [
    {
      path: "administration/roles",
      operation: permission.PERMISSION_LIST_ROUTE.operation,
      label: "roles",
      component: () => <p>role</p>,
    },
    {
      path: "administration/users",
      operation: "wamn-control:user/list@0.9.0",
      label: "users",
      component: () => <p>user</p>,
    },
  ],
};
const CONTROL_SECTIONS: readonly ShellSection[] = [
  {
    label: "Org",
    screens: [
      { path: "org", operation: user.USER_LIST_ROUTE.operation, label: "org", component: () => <p>members</p> },
    ],
  },
  {
    label: "Projects",
    screens: [
      {
        path: "projects/:project",
        operation: member.MEMBER_LIST_ROUTE.operation,
        label: "project",
        component: (props) => <p>members of {props.params.project}</p>,
      },
    ],
  },
];

export const ONE = [{ aud: AUD, org: "acme", project: "widgets", env: "dev" }];

/** What `permission.mine` answers an admin. */
export const ADMIN = { admin: true, permissions: [] };

export const CONTROL = "urn:wamn:control:acme";

/** What `control.mine` answers an org-admin of two projects. */
const ORG_ADMIN = {
  org_admin: true,
  projects: [
    { project: "gadgets", project_admin: true },
    { project: "widgets", project_admin: true },
  ],
};

/**
 * The identity service, holding one session cookie or none, with the
 * environments it lists, and the application host's `permission.mine`.
 */
export function identity(
  signedIn: boolean,
  listed: readonly unknown[] = ONE,
  mine: unknown = ADMIN,
  control: unknown = ORG_ADMIN,
) {
  const state = {
    signedIn,
    calls: [] as string[],
    bodies: [] as unknown[],
    queries: [] as Record<string, string>[],
    requests: [] as (RequestInit | undefined)[],
  };
  const times = () =>
    new Response(
      JSON.stringify({
        sub: "00000000-0000-4000-8000-0000000000a1",
        expires_at: Date.now() / 1000 + 3600,
        login_expires_at: Date.now() / 1000 + 86400,
      }),
      { status: 200 },
    );
  const fetch = (url: string | URL | Request, init?: RequestInit) => {
    const address = new URL(String(url), "http://fixture");
    const path = address.pathname;
    state.queries.push(Object.fromEntries(address.searchParams));
    state.calls.push(path);
    state.requests.push(init);
    state.bodies.push(init?.body === undefined ? undefined : JSON.parse(String(init.body)));
    if (path.startsWith("/api/wamn_control/permission/mine")) {
      return Promise.resolve(new Response(JSON.stringify([{ value: mine }]), { status: 200 }));
    }
    // A control session calls its host with no prefix.
    if (path.startsWith("/api/wamn_control/control/mine")) {
      return Promise.resolve(
        new Response(JSON.stringify([{ value: { ...(control as object), next_cursor: null, total: null } }]), {
          status: 200,
        }),
      );
    }
    switch (path) {
      case "/password/environments":
        return Promise.resolve(
          new Response(JSON.stringify({ environments: listed, next_cursor: null, total: listed.length }), {
            status: 200,
          }),
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

export function open(path: string, fetch: typeof globalThis.fetch) {
  window.history.pushState({}, "", path);
  return render(() => (
    <Shell
      title="Stub app"
      org="acme"
      sections={[...SECTIONS, ADMINISTRATION]}
      controlSections={CONTROL_SECTIONS}
      fetch={fetch}
    />
  ));
}

/** Signs in once the form shows, which is after the renewal answers. */
export async function signIn() {
  input(await screen.findByLabelText("Email"), "someone@wamn.dev");
  input(screen.getByLabelText("Password"), "a long password string");
  flush();
  fireEvent.click(screen.getByRole("button", { name: "Sign in" }));
}

beforeEach(() => {
  // A saved user of an earlier test would reopen offline in the next one.
  localStorage.clear();
  // jsdom does not scroll elements, but the menu scrolls its keyboard target.
  HTMLElement.prototype.scrollIntoView = () => undefined;
  // jsdom has no media queries, and the sidebar asks whether the page is narrow.
  window.matchMedia = ((query: string) => ({
    matches: false,
    media: query,
    addEventListener: () => undefined,
    removeEventListener: () => undefined,
  })) as unknown as typeof window.matchMedia;
});

afterEach(cleanup);

/** Browser input events settle before the next user event. */
export function input(element: HTMLElement, value: string): void {
  fireEvent.input(element, { target: { value } });
  flush();
}
