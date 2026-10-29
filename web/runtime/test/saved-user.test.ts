import { afterEach, expect, it, vi } from "vitest";
import {
  durationMs,
  offlineSettings,
  opensOffline,
  savedUser,
  savedView,
  LOCAL_FIRST_DEFAULTS,
  SESSION_DEFAULTS,
} from "../src/saved-user.js";

/** The runtime tests run in Node, which has no local storage. */
function stubStorage(): void {
  const items = new Map<string, string>();
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => items.get(key) ?? null,
    setItem: (key: string, value: string) => void items.set(key, value),
    removeItem: (key: string) => void items.delete(key),
  });
}

const user = (userId: string, loginExpiresAt = 2_000) => ({ userId, expiresAt: 1_000, loginExpiresAt });

afterEach(() => {
  vi.unstubAllGlobals();
});

it("restores only the last user of the same application and environment", () => {
  stubStorage();
  savedUser("wms", "dev").save(user("first"));
  expect(savedUser("wms", "dev").load()).toEqual(user("first"));
  expect(savedUser("other", "dev").load()).toBeUndefined();
  expect(savedUser("wms", "prod").load()).toBeUndefined();
  savedUser("wms", "dev").save(user("second"));
  expect(savedUser("wms", "dev").load()?.userId).toBe("second");
  savedUser("wms", "dev").clear();
  expect(savedUser("wms", "dev").load()).toBeUndefined();
});

it("ignores a saved user without session times", () => {
  stubStorage();
  localStorage.setItem("wamn-user:wms:dev", "first");
  expect(savedUser("wms", "dev").load()).toBeUndefined();
});

it("allows the page to open when local storage refuses access", () => {
  const denied = () => {
    throw new Error("denied");
  };
  vi.stubGlobal("localStorage", { getItem: denied, setItem: denied, removeItem: denied });
  expect(savedUser("wms", "dev").load()).toBeUndefined();
  expect(() => savedUser("wms", "dev").save(user("user"))).not.toThrow();
  expect(() => savedUser("wms", "dev").clear()).not.toThrow();
  expect(savedView("wms", "dev").load("user")).toBeUndefined();
  expect(() => savedView("wms", "dev").save({ userId: "user", held: [], admin: false })).not.toThrow();
});

it("restores the saved view only for the user who saved it", () => {
  stubStorage();
  const view = { userId: "first", held: ["receiving.load_open_receipt_lines"], admin: false };
  savedView("wms", "dev").save(view);
  expect(savedView("wms", "dev").load("first")).toEqual(view);
  expect(savedView("wms", "dev").load("second")).toBeUndefined();
  expect(savedView("wms", "prod").load("first")).toBeUndefined();
});

it("uses the session defaults when no settings are saved", () => {
  expect(offlineSettings(undefined)).toEqual(SESSION_DEFAULTS);
  expect(offlineSettings(user("first"))).toEqual(SESSION_DEFAULTS);
  expect(offlineSettings({ ...user("first"), settings: LOCAL_FIRST_DEFAULTS })).toEqual(LOCAL_FIRST_DEFAULTS);
});

it("opens offline under session only before the login expires, and always under local_first", () => {
  expect(opensOffline(user("first", 2_000), 1_999_999)).toBe(true);
  expect(opensOffline(user("first", 2_000), 2_000_000)).toBe(false);
  expect(opensOffline({ ...user("first", 2_000), settings: LOCAL_FIRST_DEFAULTS }, 9_000_000)).toBe(true);
});

it("reads ISO 8601 durations in milliseconds", () => {
  expect(durationMs("PT1H")).toBe(3_600_000);
  expect(durationMs("P1DT2H3M4.5S")).toBe(93_784_500);
  expect(durationMs("PT90S")).toBe(90_000);
  for (const bad of ["P", "PT", "1H", "PT1X", "P1W"]) expect(() => durationMs(bad)).toThrow();
});
