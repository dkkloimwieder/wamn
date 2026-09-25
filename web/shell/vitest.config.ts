import { fileURLToPath } from "node:url";

import solid from "vite-plugin-solid";
import { defineConfig } from "vitest/config";

/**
 * The shell tests render SolidJS in a document that jsdom supplies.
 *
 * The aliases resolve the runtime and the UI package by name, exactly as
 * `tsconfig.json` maps them for the type check. `web/ui` installs its own
 * libraries, and they import solid-js. Two copies of solid-js break context
 * and reactivity, so every dependency is inlined and every import of solid-js
 * resolves to this package's copy.
 */
export default defineConfig({
  plugins: [solid()],
  resolve: {
    conditions: ["development", "browser"],
    alias: {
      "@wamn/web-runtime": fileURLToPath(new URL("../runtime/src/index.ts", import.meta.url)),
      "@wamn/ui": fileURLToPath(new URL("../ui/src/index.ts", import.meta.url)),
    },
    dedupe: ["solid-js", "@solidjs/router", "@tanstack/solid-table"],
  },
  test: {
    environment: "jsdom",
    server: { deps: { inline: true } },
  },
});
