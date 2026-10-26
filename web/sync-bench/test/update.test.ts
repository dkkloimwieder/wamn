import { expect, it } from "vitest";
import { canReload } from "../src/update.js";

it.each([
  [1, 0, 0, false],
  [0, 1, 0, false],
  [0, 0, 1, true],
  [0, 0, 0, true],
  [1, 1, 2, false],
])("reload with pending=%i sending=%i refused=%i is %s", (pending, sending, refused, allowed) => {
  expect(canReload({ pending, sending, refused, lastSentAt: undefined })).toBe(allowed);
});
