/**
 * The visual test of the gallery runs in Vitest browser mode, in the Chrome
 * that the machine has, through Playwright. `pnpm run test:visual` runs it.
 *
 * It resolves the runtime, the UI package and its stylesheet as the gallery's
 * own `gallery/vite.config.ts` does, so the page it draws is the page the
 * gallery serves. A screenshot matches only when no pixel changed: the color
 * threshold is 0, and anti-aliased pixels count too.
 */

import { fileURLToPath } from "node:url";

import tailwindcss from "@tailwindcss/vite";
import { playwright } from "@vitest/browser-playwright";
import solid from "vite-plugin-solid";
import { defineConfig } from "vitest/config";

/** One path relative to this directory, as an absolute path. */
function local(path: string): string {
  return fileURLToPath(new URL(path, import.meta.url));
}

export default defineConfig({
  plugins: [solid(), tailwindcss()],
  resolve: {
    alias: [
      { find: "@wamn/web-runtime", replacement: local("../runtime/src/index.ts") },
      { find: "@wamn/ui/styles.css", replacement: local("../ui/src/styles.css") },
      { find: /^@wamn\/ui$/, replacement: local("../ui/src/index.ts") },
    ],
    dedupe: ["solid-js", "@tanstack/solid-table"],
  },
  server: {
    // The UI stylesheet names its font files by path outside this directory.
    fs: { allow: [local("..")] },
  },
  test: {
    include: ["gallery/*.visual.tsx"],
    browser: {
      enabled: true,
      headless: true,
      provider: playwright({ launchOptions: { channel: "chrome" } }),
      instances: [{ browser: "chromium", viewport: { width: 1440, height: 900 } }],
      expect: {
        toMatchScreenshot: {
          comparatorName: "pixelmatch",
          comparatorOptions: { threshold: 0, includeAA: true },
        },
      },
    },
  },
});
