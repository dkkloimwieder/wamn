import { afterEach, expect, it, vi } from "vitest";
import { savedUser, savedView } from "../src/saved-user.js";

/** The runtime tests run in Node, which has no local storage. */
function stubStorage(): void {
  const items = new Map<string, string>();
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => items.get(key) ?? null,
    setItem: (key: string, value: string) => void items.set(key, value),
  });
}

afterEach(() => {
  vi.unstubAllGlobals();
});

it("restores only the last user of the same application and environment", () => {
  stubStorage();
  savedUser("wms", "dev").save("first");
  expect(savedUser("wms", "dev").load()).toBe("first");
  expect(savedUser("other", "dev").load()).toBeUndefined();
  expect(savedUser("wms", "prod").load()).toBeUndefined();
  savedUser("wms", "dev").save("second");
  expect(savedUser("wms", "dev").load()).toBe("second");
});

it("allows the page to open when local storage refuses access", () => {
  const denied = () => {
    throw new Error("denied");
  };
  vi.stubGlobal("localStorage", { getItem: denied, setItem: denied });
  expect(savedUser("wms", "dev").load()).toBeUndefined();
  expect(() => savedUser("wms", "dev").save("user")).not.toThrow();
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
