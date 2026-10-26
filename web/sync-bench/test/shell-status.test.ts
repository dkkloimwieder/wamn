import { expect, it } from "vitest";
import { shellStatus } from "../src/shell-status";

it("shows a pending startup without claiming that storage was denied", () => {
  expect(shellStatus()).toBe("Worker: starting. Persisted: pending. Quota: pending.");
});

it("shows the registered worker and granted storage estimate", () => {
  expect(shellStatus({ worker: true, persisted: true, quota: { usage: 1024, quota: 8192 } })).toBe(
    "Worker: registered. Persisted: yes. Storage protected. Quota: 8192 bytes. Used: 1024 bytes.",
  );
});

it("shows unavailable APIs and denied storage without hiding zero estimates", () => {
  expect(shellStatus({ worker: false, persisted: false, quota: { usage: 0, quota: 0 } })).toBe(
    "Worker: unavailable. Persisted: no. Storage not protected. Quota: 0 bytes. Used: 0 bytes.",
  );
});
