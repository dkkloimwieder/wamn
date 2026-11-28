import { fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { createTransport } from "@wamn/web-runtime";
import { expect, it, vi } from "vitest";
import { Landing } from "../src/landing";
import { CONTROL, identity, open, signIn } from "./fixtures";

const apex = "acme.wamn.example";
const preview = `preview.${apex}`;
const receiving = {
  env: "prod",
  prod: true,
  project: "stock",
  app: "receiving",
  url: `https://${apex}/stock/receiving/`,
};
const shipping = { ...receiving, app: "shipping", url: `https://${apex}/stock/shipping/` };
const previewApp = { ...receiving, env: "preview", prod: false, url: `https://${preview}/stock/receiving/` };
type App = typeof receiving;
type Choice = Pick<App, "project" | "app">;

function mount(apps: App[], choice: Choice | null, options: { host?: string; admin?: boolean } = {}) {
  const page = new URL(`https://${options.host ?? apex}/`);
  const openApp = vi.fn();
  const navigate = vi.fn();
  const signOut = vi.fn(async () => {});
  const writes = vi.fn();
  const fetcher: typeof fetch = async (input, init) => {
    const request = new Request(new URL(String(input), page.origin), init);
    switch (new URL(request.url).pathname) {
      case "/api/wamn_control/app/mine":
        return Response.json([{ value: { default: choice, apps } }]);
      case "/api/wamn_control/app/default": {
        const body = await request.json();
        writes(request.method, body);
        return Response.json([{ request_id: body[0].request_id, value: {} }]);
      }
      default:
        return new Response(null, { status: 404 });
    }
  };
  render(() => (
    <Landing
      page={page}
      navigate={navigate}
      aud={CONTROL}
      transport={createTransport({ baseUrl: "/api", cookie: true, fetch: fetcher })}
      orgAdmin={options.admin ?? false}
      signOut={signOut}
      openApp={openApp}
    />
  ));
  return { openApp, signOut, writes };
}

it("opens the only app on the current host", async () => {
  const { openApp } = mount([receiving, previewApp], null);
  await waitFor(() => expect(openApp).toHaveBeenCalledWith(receiving.url));
});

it("opens the default from several apps on the current host", async () => {
  const { openApp } = mount([receiving, shipping], shipping);
  await waitFor(() => expect(openApp).toHaveBeenCalledWith(shipping.url));
});

it("lets the member choose an app when the default is on another host", async () => {
  const remote = { ...previewApp, app: "counting", url: `https://${preview}/stock/counting/` };
  const { openApp, writes } = mount([receiving, shipping, remote], remote);
  const link = await screen.findByRole("link", { name: /shipping/ });
  expect(screen.queryByRole("link", { name: /counting/ })).toBeNull();
  expect(openApp).not.toHaveBeenCalled();
  fireEvent.click(within(link.closest("li")!).getByRole("button", { name: "Make default" }));
  await waitFor(() =>
    expect(writes).toHaveBeenCalledWith("PUT", [
      { request_id: expect.any(String), value: { project: shipping.project, app: shipping.app } },
    ]),
  );
  await waitFor(() => expect(openApp).toHaveBeenCalledWith(shipping.url));
});

it("shows the empty state and offers sign-out on an env host with no eligible apps", async () => {
  const { openApp, signOut } = mount([previewApp, receiving], null, { host: `qa.${apex}` });
  expect(await screen.findByText("No application is available on this host.")).toBeDefined();
  expect(screen.queryByRole("link", { name: /receiving/ })).toBeNull();
  expect(openApp).not.toHaveBeenCalled();
  expect(screen.queryByRole("link", { name: "Org console" })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Sign out" }));
  expect(signOut).toHaveBeenCalled();
});

it("shows the empty state and links to the env app from the apex, even when it is the default", async () => {
  const { openApp } = mount([previewApp], previewApp);
  const link = await screen.findByRole("link", { name: /receiving/ });
  expect(link.getAttribute("href")).toBe(previewApp.url);
  expect(screen.getByText("No application is available on this host.")).toBeDefined();
  expect(screen.getByRole("button", { name: "Sign out" })).toBeDefined();
  expect(screen.queryByRole("button", { name: "Make default" })).toBeNull();
  expect(openApp).not.toHaveBeenCalled();
});

it("opens the sole local app on an env host of an org without prod", async () => {
  const qaApp = { ...previewApp, env: "qa", url: `https://qa.${apex}/stock/receiving/` };
  const { openApp } = mount([previewApp, qaApp], null, { host: preview });
  await waitFor(() => expect(openApp).toHaveBeenCalledWith(previewApp.url));
});

it("links to the org console for an org admin in the chooser", async () => {
  mount([receiving, shipping], null, { admin: true });
  const link = await screen.findByRole("link", { name: "Org console" });
  expect(link.getAttribute("href")).toBe(`/${CONTROL}`);
});

it("opens the landing page at the host root after sign-in and uses control.mine for the console link", async () => {
  const session = identity(false);
  const fetcher: typeof fetch = async (input, init) => {
    if (String(input) === "/api/wamn_control/app/mine") {
      return Response.json([{ value: { default: null, apps: [] } }]);
    }
    return session.fetch(input, init);
  };
  open("/", fetcher);
  await signIn();
  const link = await screen.findByRole("link", { name: "Org console" });
  expect(link.getAttribute("href")).toBe(`/${CONTROL}`);
  expect(window.location.pathname).toBe("/");
});
