import { afterEach, expect, it, vi } from "vitest";
import { savedUser } from "../src/saved-user.js";

afterEach(() => {
  localStorage.clear();
  vi.restoreAllMocks();
});

it("restores only the last user of the same application and environment", () => {
  savedUser("wms", "dev").save("first");
  expect(savedUser("wms", "dev").load()).toBe("first");
  expect(savedUser("other", "dev").load()).toBeUndefined();
  expect(savedUser("wms", "prod").load()).toBeUndefined();
  savedUser("wms", "dev").save("second");
  expect(savedUser("wms", "dev").load()).toBe("second");
});

it("allows the page to open when local storage refuses access", () => {
  vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => { throw new Error("denied"); });
  vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => { throw new Error("denied"); });
  expect(savedUser("wms", "dev").load()).toBeUndefined();
  expect(() => savedUser("wms", "dev").save("user")).not.toThrow();
});
