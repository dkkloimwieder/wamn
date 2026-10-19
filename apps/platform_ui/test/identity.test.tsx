import { fireEvent, screen, waitFor } from "@solidjs/testing-library";
import { flush } from "solid-js";
import { describe, expect, it, vi } from "vitest";
import { AUD, CONTROL, ONE, identity, input, open, signIn } from "./fixtures";

describe("identity pages", () => {
  it("posts the invitation code of the address and the password to enroll, then opens sign in", async () => {
    const seen: { url: string; body: unknown }[] = [];
    const fetch = (url: string | URL | Request, init?: RequestInit) => {
      seen.push({ url: String(url), body: JSON.parse(String(init?.body)) });
      return Promise.resolve(new Response(null, { status: 204 }));
    };
    open("/invite#ccc4533d-a81a-465d-8a44-1414369ae2fd:wamn_inv_0f", fetch as typeof globalThis.fetch);
    input(await screen.findByLabelText("email"), "someone@wamn.dev");
    input(screen.getByLabelText("password"), "a long password string");
    input(screen.getByLabelText("password again"), "a long password string");
    flush();
    fireEvent.click(screen.getByText("set password"));
    expect(await screen.findByText("sign in")).toBeDefined();
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
    fireEvent.click(await screen.findByText("forgot password"));
    const send = await screen.findByText("send reset link");
    input(screen.getByLabelText("email"), "someone@wamn.dev");
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
    input(await screen.findByLabelText("email"), "someone@wamn.dev");
    input(screen.getByLabelText("password"), "a long password string");
    input(screen.getByLabelText("password again"), "a long password string");
    flush();
    fireEvent.click(screen.getByText("set password"));
    expect(await screen.findByText("sign in")).toBeDefined();
    await waitFor(() => expect(window.location.pathname).toBe("/"));
    expect(seen).toEqual([
      {
        url: "/password/reset",
        body: { email: "someone@wamn.dev", secret: "wamn_reset_0f", password: "a long password string" },
      },
    ]);
  });

  it("signs in at the root, and the one environment of the project opens the first screen", async () => {
    const { fetch } = identity(false);
    open("/", fetch);
    await signIn();
    expect(await screen.findByText("pallets screen")).toBeDefined();
    await waitFor(() => expect(window.location.pathname).toBe(`/${AUD}/pallets`));
  });

  it("asks identity for the environments of its org and project, and offers only the reply", async () => {
    const PROD = "urn:wamn:project-env:acme:widgets:prod:q8w4e2r6";
    const { state, fetch } = identity(false, [...ONE, { aud: PROD, org: "acme", project: "widgets", env: "prod" }]);
    open("/", fetch);
    await signIn();
    expect(await screen.findByText("acme/widgets/prod")).toBeDefined();
    expect(screen.getAllByRole("button").map((button) => button.textContent)).toEqual([
      "sign in",
      "acme/widgets/dev",
      "acme/widgets/prod",
    ]);
    expect(state.bodies[state.calls.indexOf("/password/environments")]).toEqual({
      email: "someone@wamn.dev",
      password: "a long password string",
      org: "acme",
      project: "widgets",
    });
    flush();
    fireEvent.click(screen.getByText("acme/widgets/prod"));
    expect(await screen.findByText("pallets screen")).toBeDefined();
    await waitFor(() => expect(window.location.pathname).toBe(`/${PROD}/pallets`));
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
    fireEvent.click(screen.getByText("sign out"));
    expect(await screen.findByLabelText("password")).toBeDefined();
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
    expect(screen.getByText("Products").closest("a")).not.toBeNull();
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
    expect(state.calls).toContain("/wamn_control/control/mine");
    expect(state.calls.some((call) => call.includes("permission/mine"))).toBe(false);
    // No application screen and no Administration section in a control session.
    expect(screen.queryByText("Pallets")).toBeNull();
    expect(screen.queryByText("Administration")).toBeNull();
    flush();
    fireEvent.click(screen.getByText("widgets"));
    expect(await screen.findByText("members of widgets")).toBeDefined();
    await waitFor(() => expect(window.location.pathname).toBe(`/${CONTROL}/projects/widgets`));
    expect(screen.getByText("gadgets")).toBeDefined();
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
    fireEvent.click(screen.getByText("set password"));
    flush();
    await waitFor(() => expect(screen.getByRole("button").hasAttribute("disabled")).toBe(false));
    expect(fetch).not.toHaveBeenCalled();
  });

  it.each(["/invite#principal:secret", "/reset#secret"])("refuses unequal passwords at %s", async (path) => {
    const fetch = vi.fn<typeof globalThis.fetch>();
    open(path, fetch);
    input(await screen.findByLabelText("email"), "someone@wamn.dev");
    input(screen.getByLabelText("password"), "a long password string");
    input(screen.getByLabelText("password again"), "a different password");
    fireEvent.click(screen.getByText("set password"));
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
    input(await screen.findByLabelText("email"), "someone@wamn.dev");
    if (path !== "/recover") {
      input(screen.getByLabelText("password"), "a long password string");
      input(screen.getByLabelText("password again"), "a long password string");
    }
    fireEvent.click(screen.getByText(path === "/recover" ? "send reset link" : "set password"));
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
    expect(screen.getByLabelText("password")).toBeDefined();
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
    input(await screen.findByLabelText("email"), "someone@wamn.dev");
    input(screen.getByLabelText("password"), "a long password string");
    input(screen.getByLabelText("password again"), "a long password string");
    fireEvent.click(screen.getByText("set password"));
    await waitFor(() => expect(fetch).toHaveBeenCalledTimes(1));
    window.history.pushState({}, "", "/recover");
    window.dispatchEvent(new PopStateEvent("popstate"));
    await screen.findByText("send reset link");
    finish(new Response(null, { status: 204 }));
    await Promise.resolve();
    await Promise.resolve();
    flush();
    expect(window.location.pathname).toBe("/recover");
  });
});
