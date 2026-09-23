import { fileURLToPath } from "node:url";

import solid from "vite-plugin-solid";
import { defineConfig } from "vitest/config";

/**
 * The component tests render SolidJS, so the compiler runs over the test files
 * and the environment supplies a document.
 *
 * The aliases resolve the runtime and the UI package by name, exactly as
 * `tsconfig.json` maps them for the type check. Both tools need their own
 * mapping, because this package installs them from the checkout rather than
 * from a registry.
 *
 * `web/ui` installs its own libraries, and they import solid-js. Two copies of
 * solid-js break context and reactivity, so those libraries are inlined and
 * every import of solid-js and the table package resolves to this package's
 * copy.
 */
export default defineConfig({
  plugins: [solid()],
  resolve: {
    conditions: ["development", "browser"],
    alias: {
      "@wamn/web-runtime": fileURLToPath(new URL("../runtime/src/index.ts", import.meta.url)),
      "@wamn/ui": fileURLToPath(new URL("../ui/src/index.ts", import.meta.url)),
    },
    dedupe: ["solid-js", "@tanstack/solid-table"],
  },
  test: {
    environment: "jsdom",
    server: {
      deps: {
        inline: [/@kobalte/, /@corvu/, /solid-sonner/, /lucide-solid/, /@tanstack\/solid-table/],
      },
    },
  },
});
