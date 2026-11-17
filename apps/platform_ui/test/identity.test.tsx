import { fireEvent, screen, waitFor } from "@solidjs/testing-library";
import { flush } from "solid-js";
import { describe, expect, it, vi } from "vitest";
import { AUD, identity, input, open, signIn } from "./fixtures";

describe("identity pages", () => {
  it.each(["/", "/invite", "/recover", "/reset", `/${AUD}/pallets`])(
    "uses EntryPanel without breadcrumbs at %s",
    async (path) => {
      open(path, identity(false).fetch);
      const email = await screen.findByLabelText("Email");
      expect(screen.getByRole("main").contains(email)).toBe(true);
      expect(screen.getByRole("heading")).toBeTruthy();
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
    {
      const observed = seen;
      expect(observed).toHaveProperty(["length"], 1);
      expect(observed).toHaveProperty(["0", "url"], "/password/enroll");
      expect(observed).toHaveProperty(["0", "body", "principal_id"], "ccc4533d-a81a-465d-8a44-1414369ae2fd");
      expect(observed).toHaveProperty(["0", "body", "invitation"], "wamn_inv_0f");
      expect(observed).toHaveProperty(["0", "body", "password"], "a long password string");
    }
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
    await waitFor(() => expect(send).toHaveProperty("disabled", false));
    expect(screen.queryByRole("alert")).toBeNull();
    await waitFor(() => expect(window.location.pathname).toBe("/recover"));
    {
      const observed = seen;
      expect(observed).toHaveProperty(["length"], 1);
      expect(observed).toHaveProperty(["0", "url"], "/password/recover");
      expect(observed).toHaveProperty(["0", "body", "email"], "someone@wamn.dev");
    }
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
    {
      const observed = seen;
      expect(observed).toHaveProperty(["length"], 1);
      expect(observed).toHaveProperty(["0", "url"], "/password/reset");
      expect(observed).toHaveProperty(["0", "body", "email"], "someone@wamn.dev");
      expect(observed).toHaveProperty(["0", "body", "secret"], "wamn_reset_0f");
      expect(observed).toHaveProperty(["0", "body", "password"], "a long password string");
    }
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

});

describe("identity refusals", () => {
  it.each(["/invite", "/reset"])("refuses %s without a code", async (path) => {
    const fetch = vi.fn<typeof globalThis.fetch>();
    open(path, fetch);
    expect((await screen.findByRole("alert")).textContent).not.toBe("");
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
    expect(await screen.findByRole("alert")).toBeTruthy();
    expect(fetch).not.toHaveBeenCalled();
  });

  it.each(["/invite#principal:secret", "/reset#secret", "/recover"])(
    "shows the identity refusal at %s",
    async (path) => {
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
      expect(await screen.findByRole("alert")).toBeTruthy();
      expect(window.location.pathname).toBe(path.split("#")[0]);
      expect(fetch).toHaveBeenCalledTimes(1);
      expect(fetch.mock.calls[0]?.[1]?.credentials).toBe("include");
    },
  );

  it("shows a refused password without creating a session", async () => {
    const fetch = vi.fn<typeof globalThis.fetch>(async () => new Response(null, { status: 401 }));
    open("/", fetch);
    await signIn();
    const alert = await screen.findByRole("alert");
    expect(Number(alert.textContent?.match(/\d+/)?.[0])).toBe(401);
    expect(fetch).toHaveBeenCalledTimes(1);
    expect(fetch.mock.calls[0]?.[0]).toBe("/password/session");
  });

  it("shows renewal failure on the same screen address", async () => {
    const fetch = vi.fn<typeof globalThis.fetch>(async () => new Response(null, { status: 503 }));
    open(`/${AUD}/pallets`, fetch);
    const alert = await screen.findByRole("alert");
    expect(Number(alert.textContent?.match(/\d+/)?.[0])).toBe(503);
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
