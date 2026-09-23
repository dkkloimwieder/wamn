/**
 * The gallery serves one page from this directory and makes no request beyond
 * its own modules.
 *
 * The aliases resolve the runtime and the UI package from the checkout, as
 * `vitest.config.ts` does. The UI package installs its own libraries, and they
 * import solid-js, so dedupe resolves every import of solid-js and the table
 * package to this package's copy. Two copies break context and reactivity.
 */

import { fileURLToPath } from "node:url";

import tailwindcss from "@tailwindcss/vite";
import solid from "vite-plugin-solid";
import { defineConfig } from "vite";

/** One path relative to this directory, as an absolute path. */
function local(path: string): string {
  return fileURLToPath(new URL(path, import.meta.url));
}

export default defineConfig({
  root: local("."),
  plugins: [solid(), tailwindcss()],
  resolve: {
    alias: [
      { find: "@wamn/web-runtime", replacement: local("../../runtime/src/index.ts") },
      { find: "@wamn/ui/styles.css", replacement: local("../../ui/src/styles.css") },
      { find: /^@wamn\/ui$/, replacement: local("../../ui/src/index.ts") },
    ],
    // A folder alias for solid-js would select its server build, so the
    // package name resolves through dedupe instead.
    dedupe: ["solid-js", "@tanstack/solid-table"],
  },
  server: {
    // The UI stylesheet names its font files by path, and a path outside this
    // directory is refused unless it is allowed here.
    fs: { allow: [local("../..")] },
  },
});
