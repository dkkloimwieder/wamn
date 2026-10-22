import { fireEvent, screen, waitFor, within } from "@solidjs/testing-library";
import { flush } from "solid-js";
import { describe, expect, it, vi } from "vitest";
import { AUD, CONTROL, ONE, identity, input, open, signIn } from "./fixtures";

describe("identity pages", () => {
  it.each(["/", "/invite", "/recover", "/reset", `/${AUD}/pallets`])(
    "uses EntryPanel without breadcrumbs at %s",
    async (path) => {
      open(path, identity(false).fetch);
      const email = await screen.findByLabelText("Email");
      expect(email.closest(".ui-entry-panel")).not.toBeNull();
      expect(document.querySelector(".ui-dashboard")).toBeNull();
      expect(screen.getByRole("main").contains(email)).toBe(true);
      expect(email.closest(".ui-entry-panel")?.getAttribute("data-size")).toBe("viewport");
      expect(screen.getByRole("heading").textContent).toBe(
        path === "/invite"
          ? "Accept invitation"
          : path === "/reset"
            ? "Reset password"
            : path === "/recover"
              ? "Recover password"
              : "Sign in",
      );
      expect(screen.queryByRole("navigation", { name: "Platform location" })).toBeNull();
    },
  );
  it("posts the invitation code of the address and the password to enroll, then opens sign in", async () => {
    const seen: { url: string; body: unknown }[] = [];
    const fetch = (url: string | URL | Request, init?: RequestInit) => {
      seen.push({ url: String(url), body: JSON.parse(String(init?.body)) });
      return Promise.resolve(new Response(null, { status: 204 }));
    };
    open("/invite#ccc4533d-a81a-465d-8a44-1414369ae2fd:wamn_inv_0f", fetch as typeof globalThis.fetch);
    input(await screen.findByLabelText("Email"), "someone@wamn.dev");
    input(screen.getByLabelText("Password"), "a long password string");
    input(screen.getByLabelText("Password again"), "a long password string");
    flush();
    fireEvent.click(screen.getByText("Set password"));
    expect(await screen.findByRole("button", { name: "Sign in" })).toBeDefined();
    await waitFor(() => expect(window.location.pathname).toBe("/"));
    expect(seen).toEqual([
      {
        url: "/password/enroll",
        body: {
          principal_id: "ccc4533d-a81a-465d-8a44-1414369ae2fd",
          invitation: "wamn_inv_0f",
          password: "a long password string",
        },
      },
    ]);
  });

  it("links sign in to recovery, and posts the email to recover", async () => {
    const seen: { url: string; body: unknown }[] = [];
    const fetch = (url: string | URL | Request, init?: RequestInit) => {
      seen.push({ url: String(url), body: JSON.parse(String(init?.body)) });
      return Promise.resolve(new Response(null, { status: 202 }));
    };
    open("/", fetch as typeof globalThis.fetch);
    flush();
    fireEvent.click(await screen.findByText("Forgot password"));
    const send = await screen.findByText("Send reset link");
    input(screen.getByLabelText("Email"), "someone@wamn.dev");
    flush();
    fireEvent.click(send);
    expect(await screen.findByText(/a mail with a reset link is on its way/)).toBeDefined();
    await waitFor(() => expect(window.location.pathname).toBe("/recover"));
    expect(seen).toEqual([{ url: "/password/recover", body: { email: "someone@wamn.dev" } }]);
  });

  it("posts the reset code of the address, the email and the password to reset, then opens sign in", async () => {
    const seen: { url: string; body: unknown }[] = [];
    const fetch = (url: string | URL | Request, init?: RequestInit) => {
      seen.push({ url: String(url), body: JSON.parse(String(init?.body)) });
      return Promise.resolve(new Response(null, { status: 200 }));
    };
    open("/reset#wamn_reset_0f", fetch as typeof globalThis.fetch);
    input(await screen.findByLabelText("Email"), "someone@wamn.dev");
    input(screen.getByLabelText("Password"), "a long password string");
    input(screen.getByLabelText("Password again"), "a long password string");
    flush();
    fireEvent.click(screen.getByText("Set password"));
    expect(await screen.findByRole("button", { name: "Sign in" })).toBeDefined();
    await waitFor(() => expect(window.location.pathname).toBe("/"));
    expect(seen).toEqual([
      {
        url: "/password/reset",
        body: { email: "someone@wamn.dev", secret: "wamn_reset_0f", password: "a long password string" },
      },
    ]);
  });

  it("pages audiences, retains rows on 429, and never treats a partial page as the only audience", async () => {
    const session = identity(false);
    const requests: Record<string, unknown>[] = [];
    let refuse = true;
    const fetch: typeof globalThis.fetch = async (url, init) => {
      if (String(url) !== "/password/environments") return session.fetch(url, init);
      const body = JSON.parse(String(init?.body));
      requests.push(body);
      if (body.cursor && refuse) {
        refuse = false;
        return new Response("", { status: 429, headers: { "retry-after": "60" } });
      }
      return new Response(
        JSON.stringify(
          body.cursor
            ? { environments: [{ aud: CONTROL, org: "acme" }], next_cursor: null, total: null }
            : { environments: ONE, next_cursor: "control-page", total: null },
        ),
      );
    };
    open("/", fetch);
    await signIn();
    const table = await screen.findByRole("table", { name: "Audiences" });
    expect(within(table).getByRole("link", { name: "dev" })).toBeTruthy();
    expect(session.state.calls).not.toContain("/password/session");
    fireEvent.click(screen.getByRole("button", { name: "Next page" }));
    await screen.findByText(/Retry after 60 seconds/);
    expect(within(table).getByRole("link", { name: "dev" })).toBeTruthy();
    expect(requests).toHaveLength(2);
    fireEvent.click(screen.getByRole("button", { name: "Next page" }));
    await within(table).findByRole("link", { name: "Control" });
    expect(within(table).queryByRole("link", { name: "dev" })).toBeNull();
    expect((screen.getByRole("button", { name: "Next page" }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(screen.getByRole("button", { name: "Previous page" }));
    await within(table).findByRole("link", { name: "dev" });
    expect(requests.map(({ limit, cursor }) => ({ limit, cursor }))).toEqual([
      { limit: 25, cursor: undefined },
      { limit: 25, cursor: "control-page" },
      { limit: 25, cursor: "control-page" },
      { limit: 25, cursor: undefined },
    ]);
  });

  it("signs in at the root, and the one environment of the org opens the first screen", async () => {
    const { fetch } = identity(false);
    open("/", fetch);
    await signIn();
    expect(await screen.findByText("pallets screen")).toBeDefined();
    await waitFor(() => expect(window.location.pathname).toBe(`/${AUD}/pallets`));
  });

  it("asks identity for the environments of its org, and offers only the reply", async () => {
    const PROD = "urn:wamn:project-env:acme:widgets:prod:q8w4e2r6";
    const { state, fetch } = identity(false, [...ONE, { aud: PROD, org: "acme", project: "widgets", env: "prod" }]);
    open("/", fetch);
    await signIn();
    const audiences = await screen.findByRole("table", { name: "Audiences" });
    expect(
      within(audiences)
        .getAllByRole("link")
        .map((link) => link.textContent),
    ).toEqual(["dev", "prod"]);
    expect(screen.queryByLabelText("Email")).toBeNull();
    expect(screen.queryByLabelText("Password")).toBeNull();
    expect(screen.queryByRole("button", { name: "Sign in" })).toBeNull();
    expect(audiences.closest(".platform-table-screen")).not.toBeNull();
    expect(audiences.closest(".ui-dashboard")?.getAttribute("data-scroll")).toBe("content");
    expect(screen.queryByRole("navigation", { name: "Platform location" })).toBeNull();
    expect(state.bodies[state.calls.indexOf("/password/environments")]).toEqual({
      email: "someone@wamn.dev",
      password: "a long password string",
      org: "acme",
      limit: 25,
    });
    fireEvent.click(screen.getByRole("button", { name: "Refresh results" }));
    await waitFor(() => expect(state.calls.filter((path) => path === "/password/environments")).toHaveLength(2));
    flush();
    fireEvent.click(within(audiences).getByRole("link", { name: "prod" }));
    expect(await screen.findByText("pallets screen")).toBeDefined();
    await waitFor(() => expect(window.location.pathname).toBe(`/${PROD}/pallets`));
  });

  it("lists only held audiences with their org, project, and environment", async () => {
    const listed = [
      ...["widgets", "gadgets"].flatMap((project) =>
        ["dev", "prod"].map((env) => ({
          aud: `urn:wamn:project-env:acme:${project}:${env}:k3m9x2p7`,
          org: "acme",
          project,
          env,
        })),
      ),
      { aud: "urn:wamn:project-env:other:widgets:dev:q8w4e2r6", org: "other", project: "widgets", env: "dev" },
      { aud: CONTROL, org: "acme" },
    ];
    const { state, fetch } = identity(false, listed);
    open("/", fetch);
    await signIn();
    const audiences = await screen.findByRole("table", { name: "Audiences" });
    expect(
      within(audiences)
        .getAllByRole("columnheader")
        .map((cell) => cell.textContent),
    ).toEqual(["Org", "Project", "Environment"]);
    expect(
      within(audiences)
        .getAllByRole("row")
        .slice(1)
        .map((row) =>
          within(row)
            .getAllByRole("cell")
            .map((cell) => cell.textContent),
        ),
    ).toEqual([
      ["acme", "widgets", "dev"],
      ["acme", "widgets", "prod"],
      ["acme", "gadgets", "dev"],
      ["acme", "gadgets", "prod"],
      ["other", "widgets", "dev"],
      ["acme", "", "Control"],
    ]);
    expect(audiences.querySelector("details")).toBeNull();
    const links = within(audiences).getAllByRole("link");
    expect(links.map((link) => link.getAttribute("href")).sort()).toEqual(
      listed.map((entry) => `/${entry.aud}`).sort(),
    );
    fireEvent.click(links[4]!);
    // Selection uses the returned audience; the shell still enforces this deploy's org.
    expect(await screen.findByText("This address names an environment of another org.")).toBeDefined();
    await waitFor(() => expect(window.location.pathname).toBe("/urn:wamn:project-env:other:widgets:dev:q8w4e2r6"));
    expect(state.bodies[state.calls.indexOf("/password/session")]).toEqual({
      email: "someone@wamn.dev",
      password: "a long password string",
      aud: listed[4]!.aud,
      carrier: "cookie",
    });
  });

  it("shows only that no access has been granted when identity lists no environment", async () => {
    const { state, fetch } = identity(false, []);
    open("/", fetch);
    await signIn();
    expect(await screen.findByText("No access has been granted.")).toBeDefined();
    expect(screen.queryByText("Stub app")).toBeNull();
    expect(screen.queryByRole("button")).toBeNull();
    expect(screen.queryByRole("textbox")).toBeNull();
    expect(state.calls).toEqual(["/password/environments"]);
    expect(screen.getByRole("heading", { name: "No access" })).toBeTruthy();
    expect(document.querySelector(".ui-dashboard")).toBeNull();
    expect(screen.getByRole("link", { name: "Return to sign in" }).getAttribute("href")).toBe("/");
    fireEvent.click(screen.getByRole("link", { name: "Return to sign in" }));
    await screen.findByRole("button", { name: "Sign in" });
  });

  it("asks for the password on a screen address with no session, and shows that screen after it", async () => {
    const { fetch } = identity(false);
    open(`/${AUD}/products`, fetch);
    await signIn();
    expect(await screen.findByText("products screen")).toBeDefined();
    await waitFor(() => expect(window.location.pathname).toBe(`/${AUD}/products`));
  });

  it("signs out, and asks for the password on the same address", async () => {
    const { state, fetch } = identity(true);
    open(`/${AUD}/pallets`, fetch);
    await screen.findByText("pallets screen");
    flush();
    fireEvent.keyDown(screen.getByRole("button", { name: "User menu" }), { key: "Enter" });
    fireEvent.keyDown(await screen.findByRole("menuitem", { name: "Sign out" }), { key: "Enter" });
    expect(await screen.findByLabelText("Password")).toBeDefined();
    expect(screen.queryByText("pallets screen")).toBeNull();
    expect(state.signedIn).toBe(false);
    await waitFor(() => expect(window.location.pathname).toBe(`/${AUD}/pallets`));
    await signIn();
    await screen.findByText("pallets screen");
    expect(state.calls.filter((call) => call.includes("permission/mine"))).toHaveLength(2);
  });

  it("shows only the screens, actions and routes of the operations the caller holds", async () => {
    const mine = { admin: false, permissions: ["acme:product/aggregate", "acme:pallet/get"] };
    const { fetch } = identity(false, ONE, mine);
    open("/", fetch);
    await signIn();
    expect(await screen.findByText("product totals screen")).toBeDefined();
    await waitFor(() => expect(window.location.pathname).toBe(`/${AUD}/product-totals`));
    // One held screen of Products, so the model names it, and Pallets has none.
    expect(screen.getByRole("link", { name: "Products" })).not.toBeNull();
    expect(screen.queryByText("Pallets")).toBeNull();
    expect(screen.queryByText("query")).toBeNull();
    flush();
    fireEvent.click(screen.getByText("open pallet abc"));
    expect(await screen.findByText("pallet abc record")).toBeDefined();
    expect(screen.queryByText("update")).toBeNull();
  });

  it("offers the control audience of the org as Control, and shows an org-admin the org and every project", async () => {
    const { state, fetch } = identity(false, [...ONE, { aud: CONTROL, org: "acme" }]);
    open("/", fetch);
    await signIn();
    flush();
    fireEvent.click(await screen.findByText("Control"));
    expect(await screen.findByText("members")).toBeDefined();
    await waitFor(() => expect(window.location.pathname).toBe(`/${CONTROL}/org`));
    expect(state.calls).toContain("/api/wamn_control/control/mine");
    expect(state.calls.some((call) => call.includes("permission/mine"))).toBe(false);
    // No application screen and no Administration section in a control session.
    expect(screen.queryByText("Pallets")).toBeNull();
    expect(screen.queryByText("Administration")).toBeNull();
    flush();
    fireEvent.click(screen.getByRole("link", { name: "Projects: widgets" }));
    expect(await screen.findByText("members of widgets")).toBeDefined();
    await waitFor(() => expect(window.location.pathname).toBe(`/${CONTROL}/projects/widgets`));
    expect(screen.getByRole("link", { name: "Projects: gadgets" })).toBeDefined();
  });
});

describe("identity refusals", () => {
  it.each([
    ["/invite", "This address holds no invitation code. Open the link in the invitation mail."],
    ["/reset", "This address holds no reset code. Open the link in the reset mail."],
  ])("refuses %s without a code", async (path, text) => {
    const fetch = vi.fn<typeof globalThis.fetch>();
    open(path, fetch);
    expect(await screen.findByText(text)).toBeDefined();
    fireEvent.click(screen.getByText("Set password"));
    flush();
    await waitFor(() => expect(screen.getByRole("button").hasAttribute("disabled")).toBe(false));
    expect(fetch).not.toHaveBeenCalled();
  });

  it.each(["/invite#principal:secret", "/reset#secret"])("refuses unequal passwords at %s", async (path) => {
    const fetch = vi.fn<typeof globalThis.fetch>();
    open(path, fetch);
    input(await screen.findByLabelText("Email"), "someone@wamn.dev");
    input(screen.getByLabelText("Password"), "a long password string");
    input(screen.getByLabelText("Password again"), "a different password");
    fireEvent.click(screen.getByText("Set password"));
    expect(await screen.findByText("The two passwords differ.")).toBeDefined();
    expect(fetch).not.toHaveBeenCalled();
  });

  it.each([
    ["/invite#principal:secret", "Sign up failed"],
    ["/reset#secret", "Reset failed"],
    ["/recover", "Recovery failed"],
  ])("shows the identity refusal at %s", async (path, prefix) => {
    const fetch = vi.fn<typeof globalThis.fetch>(
      async () => new Response(JSON.stringify({ error: "code expired" }), { status: 400 }),
    );
    open(path, fetch);
    input(await screen.findByLabelText("Email"), "someone@wamn.dev");
    if (path !== "/recover") {
      input(screen.getByLabelText("Password"), "a long password string");
      input(screen.getByLabelText("Password again"), "a long password string");
    }
    fireEvent.click(screen.getByText(path === "/recover" ? "Send reset link" : "Set password"));
    expect(await screen.findByRole("alert")).toHaveProperty("textContent", `${prefix}: code expired.`);
    expect(window.location.pathname).toBe(path.split("#")[0]);
    expect(fetch).toHaveBeenCalledTimes(1);
    expect(fetch.mock.calls[0]?.[1]?.credentials).toBe("include");
  });

  it("shows a refused password without creating a session", async () => {
    const fetch = vi.fn<typeof globalThis.fetch>(async () => new Response(null, { status: 401 }));
    open("/", fetch);
    await signIn();
    expect(await screen.findByRole("alert")).toHaveProperty(
      "textContent",
      "Sign in failed: /password/environments answered 401.",
    );
    expect(fetch).toHaveBeenCalledTimes(1);
    expect(fetch.mock.calls[0]?.[0]).toBe("/password/environments");
  });

  it("shows renewal failure on the same screen address", async () => {
    const fetch = vi.fn<typeof globalThis.fetch>(async () => new Response(null, { status: 503 }));
    open(`/${AUD}/pallets`, fetch);
    expect(await screen.findByRole("alert")).toHaveProperty(
      "textContent",
      "The session could not be renewed: Error: /password/renew answered 503.",
    );
    expect(screen.getByLabelText("Password")).toBeDefined();
    expect(window.location.pathname).toBe(`/${AUD}/pallets`);
    expect(fetch).toHaveBeenCalledTimes(1);
  });

  it("does not navigate when a disposed password form finishes", async () => {
    let finish!: (response: Response) => void;
    const fetch = vi.fn<typeof globalThis.fetch>(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    );
    open("/reset#secret", fetch);
    input(await screen.findByLabelText("Email"), "someone@wamn.dev");
    input(screen.getByLabelText("Password"), "a long password string");
    input(screen.getByLabelText("Password again"), "a long password string");
    fireEvent.click(screen.getByText("Set password"));
    await waitFor(() => expect(fetch).toHaveBeenCalledTimes(1));
    window.history.pushState({}, "", "/recover");
    window.dispatchEvent(new PopStateEvent("popstate"));
    await screen.findByText("Send reset link");
    finish(new Response(null, { status: 204 }));
    await Promise.resolve();
    await Promise.resolve();
    flush();
    expect(window.location.pathname).toBe("/recover");
  });
});
