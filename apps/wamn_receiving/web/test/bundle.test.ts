// @vitest-environment node
import { assertChunksParse } from "@wamn/vite/bundle";
import { test } from "vitest";

test("every built chunk of Receiving, its worker included, parses as a module", async () => {
  await assertChunksParse(new URL("../vite.config.ts", import.meta.url));
}, 180_000);
