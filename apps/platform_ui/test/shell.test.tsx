import { cleanup, fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { describe, expect, it, vi } from "vitest";
import { Shell } from "../src/shell";
import { AUD, CONTROL, MOUNT, ADMIN, identity, open } from "./fixtures";

describe("the app shell", () => {
  it.each([CONTROL, AUD])("renders an empty audience layout at %s and refuses unknown paths", async (aud) => {
    const { fetch } = identity(true);
    const home = aud === CONTROL ? `/${aud}` : MOUNT;
    window.history.pushState({}, "", home);
    render(() => (
      <Shell title="Stub app" org="acme" {...(aud === AUD ? { mount: MOUNT } : {})} sections={[]} fetch={fetch} />
    ));
    expect(await screen.findByText(aud === CONTROL ? "ACME" : "STOCK")).toBeDefined();
    expect(await screen.findByRole("button", { name: "User menu" })).toBeDefined();
    expect(screen.getByRole("link", { name: "Choose an audience" }).getAttribute("href")).toBe("/");
    expect(screen.queryByRole("navigation", { name: "Primary navigation" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Navigation" })).toBeNull();
    expect(screen.getAllByRole("heading", { level: 1 })).toHaveLength(1);
    expect(screen.queryByRole("combobox", { name: "Audience" })).toBeNull();
    expect(screen.queryByRole("link", { name: "Go to audience home" })).toBeNull();
    window.history.pushState({}, "", `/${aud}/unknown`);
    window.dispatchEvent(new PopStateEvent("popstate"));
    expect(await screen.findByRole("link", { name: "Go to audience home" })).toBeDefined();
    expect(screen.getByRole("link", { name: "Go to audience home" }).getAttribute("href")).toBe(home);
    window.history.pushState({}, "", home);
    window.dispatchEvent(new PopStateEvent("popstate"));
    expect(await screen.findByRole("button", { name: "User menu" })).toBeDefined();
    expect(screen.queryByRole("link", { name: "Go to audience home" })).toBeNull();
  });

  it("closes navigation after browser history changes the accepted page", async () => {
    const { fetch } = identity(true);
    open(`/${AUD}/pallets`, fetch);
    await screen.findByText("pallets screen");
    const toggle = screen.getByRole("button", { name: "Navigation" });
    fireEvent.click(toggle);
    await waitFor(() => expect(toggle.getAttribute("aria-expanded")).toBe("true"));
    window.history.pushState({}, "", `/${AUD}/products`);
    window.dispatchEvent(new PopStateEvent("popstate"));
    await screen.findByText("products screen");
    await waitFor(() => expect(toggle.getAttribute("aria-expanded")).toBe("false"));
    expect(screen.queryByRole("heading")).toBeNull();
    expect(screen.queryByRole("combobox", { name: "Audience" })).toBeNull();
  });

  it("refuses a control address that names another org", async () => {
    const { state, fetch } = identity(false);
    open("/urn:wamn:org:other/projects", fetch);
    expect(await screen.findByRole("link", { name: "Choose an audience" })).toBeDefined();
    expect(screen.getByRole("link", { name: "Choose an audience" }).getAttribute("href")).toBe("/");
    {
      const observed = state.calls;
      expect(observed).toHaveProperty(["length"], 0);
    }
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
    expect(screen.queryByLabelText("Password")).toBeNull();
    {
      const observed = state.calls;
      expect(observed).toHaveProperty(["length"], 2);
      expect(observed).toHaveProperty(["0"], "/password/renew");
      expect(observed).toHaveProperty(["1"], `${MOUNT}api/wamn_control/permission/mine`);
    }
  });

  it("uses cookies and keeps no token in browser storage", async () => {
    const storage = vi.spyOn(Storage.prototype, "setItem");
    try {
      const { state, fetch } = identity(true);
      open(`/${AUD}/pallets`, fetch);
      await screen.findByText("pallets screen");
      {
        const observed = state.bodies[0];
        expect(observed).toHaveProperty(["aud"], CONTROL);
      }
      expect(state.requests[0]?.credentials).toBe("include");
      expect(new Headers(state.requests[1]?.headers).has("authorization")).toBe(false);
      // Only the saved user and view, for an offline reopen. No token.
      {
        const observed = storage.mock.calls.map(([key]) => key).sort();
        expect(observed).toHaveProperty(["length"], 2);
        expect(observed).toHaveProperty(["0"], `wamn-user:${MOUNT}`);
        expect(observed).toHaveProperty(["1"], `wamn-view:${MOUNT}`);
      }
    } finally {
      storage.mockRestore();
    }
  });

  it("names a model with one screen by the model, and lists the screens of a model with more", async () => {
    const { fetch } = identity(true);
    open(`/${AUD}/pallets`, fetch);
    await screen.findByText("pallets screen");
    const navigation = within(screen.getByRole("navigation", { name: "Primary navigation" }));
    const link = (text: string) => navigation.queryByRole("link", { name: text });
    expect(link("Pallets")?.getAttribute("aria-current")).toBe("page");
    expect(link("Products")).toBeNull();
    expect(navigation.getAllByRole("link", { name: "Products: query" })).toHaveLength(1);
    fireEvent.click(navigation.getByRole("link", { name: "Products: aggregate" }));
    expect(await screen.findByText("product totals screen")).toBeDefined();
    await waitFor(() => expect(window.location.pathname).toBe(`/${AUD}/product-totals`));
    expect(link("Products: aggregate")?.getAttribute("aria-current")).toBe("page");
    expect(link("Pallets")?.getAttribute("aria-current")).toBeNull();
    fireEvent.click(navigation.getByRole("link", { name: "Products: query" }));
    expect(await screen.findByText("products screen")).toBeDefined();
    await waitFor(() => expect(window.location.pathname).toBe(`/${AUD}/products`));
  });

  it("opens a record page from a screen, and shows the key from the address", async () => {
    const { fetch } = identity(true);
    open(`/${AUD}/products`, fetch);
    fireEvent.click(await screen.findByText("open pallet abc"));
    expect(await screen.findByText("pallet abc record")).toBeDefined();
    await waitFor(() => expect(window.location.pathname).toBe(`/${AUD}/pallets/abc`));
    expect(
      within(screen.getByRole("navigation", { name: "Primary navigation" }))
        .getByRole("link", { name: "Pallets" })
        ?.getAttribute("aria-current"),
    ).toBe("page");
    fireEvent.click(screen.getByText("open pallet def"));
    expect(await screen.findByText("pallet def record")).toBeDefined();
    await waitFor(() => expect(window.location.pathname).toBe(`/${AUD}/pallets/def`));
  });

  it("shows a record page from its address after a reload", async () => {
    const { state, fetch } = identity(true);
    open(`/${AUD}/pallets/abc`, fetch);
    expect(await screen.findByText("pallet abc record")).toBeDefined();
    {
      const observed = state.calls;
      expect(observed).toHaveProperty(["length"], 2);
      expect(observed).toHaveProperty(["0"], "/password/renew");
      expect(observed).toHaveProperty(["1"], `${MOUNT}api/wamn_control/permission/mine`);
    }
  });

  it("shows route breadcrumbs and follows an ancestor without another session read", async () => {
    const { state, fetch } = identity(true);
    open(`/${AUD}/pallets/a%2Fb/update`, fetch);
    await screen.findByText("update a/b with no note");
    const location = within(screen.getByRole("navigation", { name: "Platform location" }));
    {
      const observed = location
        .getAllByRole("listitem")
        .map((item) => item.querySelector("a, [aria-current]")?.textContent);
      expect(observed).toHaveProperty(["length"], 6);
      expect(observed).toHaveProperty(["0"], "ACME");
      expect(observed).toHaveProperty(["1"], "WIDGETS");
      expect(observed).toHaveProperty(["2"], "STOCK");
      expect(observed).toHaveProperty(["3"], "PALLETS");
      expect(observed).toHaveProperty(["4"], "A/B");
      expect(observed).toHaveProperty(["5"], "UPDATE");
    }
    expect(location.getByText("UPDATE").getAttribute("aria-current")).toBe("page");
    fireEvent.click(location.getByRole("link", { name: "A/B" }));
    await screen.findByText("pallet a/b record");
    expect(window.location.pathname).toBe(`/${AUD}/pallets/a%2Fb`);
    {
      const observed = state.calls;
      expect(observed).toHaveProperty(["length"], 2);
      expect(observed).toHaveProperty(["0"], "/password/renew");
      expect(observed).toHaveProperty(["1"], `${MOUNT}api/wamn_control/permission/mine`);
    }
  });

  it("opens a form from an action, and the form returns to the page that opened it", async () => {
    const { fetch } = identity(true);
    open(`/${AUD}/pallets/abc`, fetch);
    fireEvent.click(await screen.findByText("update"));
    expect(await screen.findByText("update abc with hi")).toBeDefined();
    await waitFor(() => expect(window.location.pathname).toBe(`/${AUD}/pallets/abc/update`));
    fireEvent.click(screen.getByText("done"));
    expect(await screen.findByText("pallet abc record")).toBeDefined();
    await waitFor(() => expect(window.location.pathname).toBe(`/${AUD}/pallets/abc`));
  });

  it("returns from a form that no page opened to the first screen of its section", async () => {
    const { fetch } = identity(true);
    open(`/${AUD}/pallets/abc/update`, fetch);
    expect(await screen.findByText("update abc with no note")).toBeDefined();
    fireEvent.click(screen.getByText("done"));
    expect(await screen.findByText("pallets screen")).toBeDefined();
    await waitFor(() => expect(window.location.pathname).toBe(`/${AUD}/pallets`));
  });

  it("shows the Administration section only to admin, with the role grid and the user grid", async () => {
    const { fetch } = identity(true);
    open(`/${AUD}/pallets`, fetch);
    await screen.findByText("pallets screen");
    expect(screen.getByRole("link", { name: "Administration: roles" })).toBeDefined();
    fireEvent.click(screen.getByRole("link", { name: "Administration: roles" }));
    expect(await screen.findByText("role")).toBeDefined();
    await waitFor(() => expect(window.location.pathname).toBe(`/${AUD}/administration/roles`));
    fireEvent.click(screen.getByRole("link", { name: "Administration: users" }));
    expect(await screen.findByText("user")).toBeDefined();
    await waitFor(() => expect(window.location.pathname).toBe(`/${AUD}/administration/users`));
    cleanup();

    const member = identity(true, { admin: false, permissions: ["acme:pallet/query"] });
    open(`/${AUD}/administration/roles`, member.fetch);
    expect(await screen.findByRole("link", { name: "Go to audience home" })).toBeDefined();
    cleanup();
    open(`/${AUD}/pallets`, member.fetch);
    await screen.findByText("pallets screen");
    expect(screen.queryByRole("link", { name: "Administration: roles" })).toBeNull();
  });

  it("shows no page for the address of a route the caller does not hold", async () => {
    const { fetch } = identity(true, { admin: false, permissions: ["acme:pallet/get"] });
    open(`/${AUD}/pallets/abc/update`, fetch);
    expect(await screen.findByRole("link", { name: "Go to audience home" })).toBeDefined();
    expect(screen.queryByText(/update abc/)).toBeNull();
  });

  it("shows a project-admin only the projects it administers, and no org destination", async () => {
    const mine = { org_admin: false, projects: [{ project: "widgets", project_admin: true }] };
    const { fetch } = identity(true, ADMIN, mine);
    open(`/${CONTROL}`, fetch);
    expect(await screen.findByText("members of widgets")).toBeDefined();
    await waitFor(() => expect(window.location.pathname).toBe(`/${CONTROL}/projects/widgets`));
    expect(screen.queryByText("Org")).toBeNull();
    cleanup();
    open(`/${CONTROL}/org`, fetch);
    expect(await screen.findByRole("link", { name: "Go to audience home" })).toBeDefined();
    cleanup();
    open(`/${CONTROL}/projects/gadgets`, fetch);
    expect(await screen.findByRole("link", { name: "Go to audience home" })).toBeDefined();
    cleanup();
    open(`/${CONTROL}/pallets`, fetch);
    expect(await screen.findByRole("link", { name: "Go to audience home" })).toBeDefined();
  });

  it("refuses the control audience of another org", async () => {
    const { state, fetch } = identity(false);
    open("/urn:wamn:org:other/org", fetch);
    expect(await screen.findByRole("link", { name: "Choose an audience" })).toBeDefined();
    {
      const observed = state.calls;
      expect(observed).toHaveProperty(["length"], 0);
    }
  });

  it.each([`/${AUD}/nothing/here`, "/unknown", "//unknown"])(
    "shows a standalone not-found page at %s",
    async (path) => {
      const { fetch } = identity(true);
      open(`${window.location.origin}${path}`, fetch);
      await screen.findByRole("heading", { name: "No page here" });

      expect(screen.getAllByRole("heading")).toHaveLength(1);
      expect(screen.getAllByRole("link")).toHaveLength(1);
      expect(screen.getByRole("link").getAttribute("href")).toBe(path.startsWith(MOUNT) ? MOUNT : "/");
    },
  );
});
