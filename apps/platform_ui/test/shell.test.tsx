import { cleanup, fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { describe, expect, it, vi } from "vitest";
import { Shell } from "../src/shell";
import { AUD, CONTROL, ONE, ADMIN, identity, open } from "./fixtures";

describe("the app shell", () => {
  it.each(["/", "/invite", "/recover", "/reset", `/${AUD}/pallets`])(
    "scopes every shell page under one UI root at %s",
    async (path) => {
      const { fetch } = identity(true);
      const { container } = open(path, fetch);
      await waitFor(() => expect(container.textContent).not.toBe(""));
      expect(container.firstElementChild?.hasAttribute("data-ui")).toBe(true);
      expect(container.querySelectorAll(":scope > [data-ui]")).toHaveLength(1);
      expect(container.firstElementChild?.getAttribute("data-ui-density")).toBe("compact");
      expect(container.firstElementChild?.hasAttribute("data-ui-theme")).toBe(false);
    },
  );

  it.each([CONTROL, AUD])("renders an empty audience layout at %s and refuses unknown paths", async (aud) => {
    const { fetch } = identity(true);
    window.history.pushState({}, "", `/${aud}`);
    const { container } = render(() => <Shell title="Stub app" org="acme" sections={[]} fetch={fetch} />);
    expect(await screen.findByText(aud === CONTROL ? "acme" : "Stub app")).toBeDefined();
    expect(await screen.findByRole("button", { name: "User menu" })).toBeDefined();
    expect(container.querySelector("nav .ui-dashboard-panel")).toBeNull();
    expect(screen.queryByText("This address names no page.")).toBeNull();
    window.history.pushState({}, "", `/${aud}/unknown`);
    window.dispatchEvent(new PopStateEvent("popstate"));
    expect(await screen.findByText("This address names no page.")).toBeDefined();
  });

  it("refuses an address that names an environment of another org", async () => {
    const { state, fetch } = identity(false);
    open("/urn:wamn:project-env:other:widgets:dev:k3m9x2p7/pallets", fetch);
    expect(await screen.findByText("This address names an environment of another org.")).toBeDefined();
    expect(state.calls).toEqual([]);
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
    expect(state.calls).toEqual(["/password/renew", "/api/wamn_control/permission/mine"]);
  });

  it("uses the cookie carrier and writes nothing to browser storage", async () => {
    const storage = vi.spyOn(Storage.prototype, "setItem");
    try {
      const { state, fetch } = identity(true);
      open(`/${AUD}/pallets`, fetch);
      await screen.findByText("pallets screen");
      expect(state.bodies[0]).toEqual({ aud: AUD, carrier: "cookie" });
      expect(state.requests[0]?.credentials).toBe("include");
      expect(new Headers(state.requests[1]?.headers).has("authorization")).toBe(false);
      expect(storage).not.toHaveBeenCalled();
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
    expect(document.querySelectorAll(".ui-dashboard-menu")).toHaveLength(1);
    expect(document.querySelector(".ui-dashboard-menu .ui-dashboard-panel")).toBeNull();
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
    expect(state.calls).toEqual(["/password/renew", "/api/wamn_control/permission/mine"]);
  });

  it("shows route breadcrumbs and follows an ancestor without another session read", async () => {
    const { state, fetch } = identity(true);
    open(`/${AUD}/pallets/a%2Fb/update`, fetch);
    await screen.findByText("update a/b with no note");
    const location = within(screen.getByRole("navigation", { name: "Platform location" }));
    expect(
      location.getAllByRole("listitem").map((item) => item.querySelector("a, [aria-current]")?.textContent),
    ).toEqual(["Stub app", "Pallets", "a/b", "update"]);
    expect(location.getByText("update").getAttribute("aria-current")).toBe("page");
    fireEvent.click(location.getByRole("link", { name: "a/b" }));
    await screen.findByText("pallet a/b record");
    expect(window.location.pathname).toBe(`/${AUD}/pallets/a%2Fb`);
    expect(state.calls).toEqual(["/password/renew", "/api/wamn_control/permission/mine"]);
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

    const member = identity(true, ONE, { admin: false, permissions: ["acme:pallet/query"] });
    open(`/${AUD}/administration/roles`, member.fetch);
    expect(await screen.findByText("This address names no page.")).toBeDefined();
    cleanup();
    open(`/${AUD}/pallets`, member.fetch);
    await screen.findByText("pallets screen");
    expect(screen.queryByRole("link", { name: "Administration: roles" })).toBeNull();
  });

  it("shows no page for the address of a route the caller does not hold", async () => {
    const { fetch } = identity(true, ONE, { admin: false, permissions: ["acme:pallet/get"] });
    open(`/${AUD}/pallets/abc/update`, fetch);
    expect(await screen.findByText("This address names no page.")).toBeDefined();
    expect(screen.queryByText(/update abc/)).toBeNull();
  });

  it("shows a project-admin only the projects it administers, and no org destination", async () => {
    const mine = { org_admin: false, projects: [{ project: "widgets", project_admin: true }] };
    const { fetch } = identity(true, ONE, ADMIN, mine);
    open(`/${CONTROL}`, fetch);
    expect(await screen.findByText("members of widgets")).toBeDefined();
    await waitFor(() => expect(window.location.pathname).toBe(`/${CONTROL}/projects/widgets`));
    expect(screen.queryByText("Org")).toBeNull();
    cleanup();
    open(`/${CONTROL}/org`, fetch);
    expect(await screen.findByText("This address names no page.")).toBeDefined();
    cleanup();
    open(`/${CONTROL}/projects/gadgets`, fetch);
    expect(await screen.findByText("This address names no page.")).toBeDefined();
    cleanup();
    open(`/${CONTROL}/pallets`, fetch);
    expect(await screen.findByText("This address names no page.")).toBeDefined();
  });

  it("refuses the control audience of another org", async () => {
    const { state, fetch } = identity(false);
    open("/urn:wamn:control:other/org", fetch);
    expect(await screen.findByText("This address names an environment of another org.")).toBeDefined();
    expect(state.calls).toEqual([]);
  });

  it("shows no page for an address that names none", async () => {
    const { fetch } = identity(true);
    open(`/${AUD}/nothing/here`, fetch);
    expect(await screen.findByText("This address names no page.")).toBeDefined();
  });
});
