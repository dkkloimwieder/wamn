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
  savedUser("/warehouse/wms/").save(user("first"));
  expect(savedUser("/warehouse/wms/").load()).toEqual(user("first"));
  expect(savedUser("/other/wms/").load()).toBeUndefined();
  expect(savedUser("/warehouse/receiving/").load()).toBeUndefined();
  savedUser("/warehouse/wms/").save(user("second"));
  expect(savedUser("/warehouse/wms/").load()?.userId).toBe("second");
  savedUser("/warehouse/wms/").clear();
  expect(savedUser("/warehouse/wms/").load()).toBeUndefined();
});

it("ignores a saved user without session times", () => {
  stubStorage();
  localStorage.setItem("wamn-user:/warehouse/wms/", "first");
  expect(savedUser("/warehouse/wms/").load()).toBeUndefined();
});

it("allows the page to open when local storage refuses access", () => {
  const denied = () => {
    throw new Error("denied");
  };
  vi.stubGlobal("localStorage", { getItem: denied, setItem: denied, removeItem: denied });
  expect(savedUser("/warehouse/wms/").load()).toBeUndefined();
  expect(() => savedUser("/warehouse/wms/").save(user("user"))).not.toThrow();
  expect(() => savedUser("/warehouse/wms/").clear()).not.toThrow();
  expect(savedView("/warehouse/wms/").load("user")).toBeUndefined();
  expect(() => savedView("/warehouse/wms/").save({ userId: "user", held: [], admin: false })).not.toThrow();
});

it("restores the saved view only for the user who saved it", () => {
  stubStorage();
  const view = { userId: "first", held: ["receiving.load_open_receipt_lines"], admin: false };
  savedView("/warehouse/wms/").save(view);
  expect(savedView("/warehouse/wms/").load("first")).toEqual(view);
  expect(savedView("/warehouse/wms/").load("second")).toBeUndefined();
  expect(savedView("/warehouse/receiving/").load("first")).toBeUndefined();
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
