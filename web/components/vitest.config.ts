import { fileURLToPath } from "node:url";

import solid from "vite-plugin-solid";
import { defineConfig } from "vitest/config";

/**
 * The component tests render SolidJS, so the compiler runs over the test files
 * and the environment supplies a document.
 *
 * The alias resolves the runtime by name, exactly as `tsconfig.json` maps it
 * for the type check. Both tools need their own mapping, because this package
 * installs the runtime from the checkout rather than from a registry.
 */
export default defineConfig({
  plugins: [solid()],
  resolve: {
    conditions: ["development", "browser"],
    alias: {
      "@wamn/web-runtime": fileURLToPath(new URL("../runtime/src/index.ts", import.meta.url)),
    },
  },
  test: { environment: "jsdom" },
});
