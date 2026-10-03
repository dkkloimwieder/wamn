/**
 * The dev server of the shell browser test. It serves `index.html` and gives
 * the page one origin, as the edge does: `/password` goes to the identity
 * stand-in, `/api` to the application host without the prefix, and
 * `/wamn_control` to the control host.
 *
 * `tests/integration/src/shell_browser_live.rs` starts it with the addresses
 * in `WAMN_BROWSER_IDENTITY_URL`, `WAMN_BROWSER_APPLICATION_URL` and
 * `WAMN_BROWSER_CONTROL_URL`, and the org and the project in
 * `WAMN_BROWSER_ORG` and `WAMN_BROWSER_PROJECT`. The server answers them at
 * `/config.json`, as the upload writes the file beside `index.html`.
 */

import { fileURLToPath } from "node:url";

import solid from "vite-plugin-solid";
import { defineConfig } from "vite";

/** One path relative to this directory, as an absolute path. */
function local(path: string): string {
  return fileURLToPath(new URL(path, import.meta.url));
}

function required(name: string): string {
  const value = process.env[name];
  if (value === undefined || value === "") {
    throw new Error(`set ${name}`);
  }
  return value;
}

const CLIENTS = "../../../../crates/catalog/model/src/host_route";

export default defineConfig({
  root: local("."),
  plugins: [
    solid(),
    {
      name: "wamn-scope",
      configureServer(server) {
        const scope = JSON.stringify({ org: required("WAMN_BROWSER_ORG"), project: required("WAMN_BROWSER_PROJECT") });
        server.middlewares.use("/config.json", (_request, response) => {
          response.setHeader("content-type", "application/json");
          response.end(scope);
        });
      },
    },
  ],
  resolve: {
    alias: [
      { find: "@wamn/web-runtime", replacement: local("../../../runtime/src/index.ts") },
      { find: /^@wamn\/ui$/, replacement: local("../../../ui/src/index.ts") },
      { find: /^@wamn\/ui\/admin$/, replacement: local("../../../ui/src/admin/index.ts") },
      { find: /^@wamn\/control-client$/, replacement: local(`${CLIENTS}/generated/client-ts/index.ts`) },
      { find: /^@wamn\/control-org-client$/, replacement: local(`${CLIENTS}/control/generated/client-ts/index.ts`) },
    ],
    // web/ui installs its own libraries, and they import solid-js. Two
    // copies of solid-js break context and reactivity.
    dedupe: ["solid-js", "@solidjs/router", "@tanstack/solid-table"],
  },
  server: {
    strictPort: true,
    fs: { allow: [local("../../../.."), local("../../../../node_modules")] },
    proxy: {
      "/password": { target: required("WAMN_BROWSER_IDENTITY_URL") },
      "/api": {
        target: required("WAMN_BROWSER_APPLICATION_URL"),
        rewrite: (path) => path.replace(/^\/api/, ""),
      },
      "/wamn_control": { target: required("WAMN_BROWSER_CONTROL_URL") },
    },
  },
});
