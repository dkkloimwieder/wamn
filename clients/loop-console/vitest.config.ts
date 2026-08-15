import solid from "@solidjs/vite-plugin";
import { defineConfig } from "vitest/config";

/**
 * Client only — never SSR. Tests load the same build the console ships, so
 * the reactive graph is live and behaviour is assertable. Per
 * https://v2.solidjs.com/guides/testing.
 */
export default defineConfig({
  plugins: [solid()],
  test: {
    environment: "jsdom",
    globals: false,
    setupFiles: ["./vitest-setup.ts"],
  },
});
