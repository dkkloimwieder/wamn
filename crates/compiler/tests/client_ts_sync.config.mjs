import { fileURLToPath } from "node:url";
import { createRequire } from "node:module";

const require = createRequire(new URL("../../../../web/sync-bench/package.json", import.meta.url));

export default {
  root: fileURLToPath(new URL("../../../../web/sync-bench/", import.meta.url)),
  resolve: {
    alias: Object.fromEntries(
      ["fake-indexeddb/auto", "@solidjs/testing-library", "@wfe/sync"].map((name) => [
        name,
        require.resolve(name).replace(/\.cjs$/, ".js"),
      ]),
    ),
    conditions: ["browser", "development"],
    dedupe: ["solid-js", "@solidjs/web", "@solidjs/signals", "@tanstack/db"],
  },
  test: {
    include: ["../../crates/schema/generator/tests/client_ts_sync.test.ts"],
    environment: "jsdom",
    server: { deps: { inline: true } },
  },
};
