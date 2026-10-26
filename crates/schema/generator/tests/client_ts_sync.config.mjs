import { fileURLToPath } from "node:url";

export default {
  root: fileURLToPath(new URL("../../../../web/sync-bench/", import.meta.url)),
  resolve: {
    conditions: ["browser", "development"],
    dedupe: ["solid-js", "@solidjs/web", "@solidjs/signals", "@tanstack/db"],
  },
  test: {
    include: ["../../crates/schema/generator/tests/client_ts_sync.test.ts"],
    environment: "jsdom",
    server: { deps: { inline: true } },
  },
};
